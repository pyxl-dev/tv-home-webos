// Garde de dépôt public (gabarit LifeOS public-repo-guard) : rien de privé n'entre dans un commit, un message, un push,
// une PR ou une issue. Copie identique dans chaque dépôt (scripts/public-guard.ts) ; la référence vit dans le gabarit.
//   bun scripts/public-guard.ts staged          lignes ajoutées à l'index (hook pre-commit)
//   bun scripts/public-guard.ts msg FICHIER     message de commit (hook commit-msg)
//   bun scripts/public-guard.ts push REMOTE     refs sur stdin comme le hook pre-push : chaque commit poussé (contenu ajouté
//                                               et message), et refus de tout commit hors de l'histoire publique
//   bun scripts/public-guard.ts tree            tous les fichiers suivis (CI)
//   bun scripts/public-guard.ts text            texte lu sur stdin
//   bun scripts/public-guard.ts claude-hook     hook PreToolUse de Claude Code : texte des commandes gh pr|issue
// Réglages du dépôt : .public-guard.json { "project": "nom", "publicRoot": "sha du premier commit public",
//   "allow": ["expressions déjà publiques par décision"] }.
// Motifs : génériques ci-dessous (publics), plus ceux propres à une installation, jamais dans le dépôt :
//   ~/.config/public-guard/forbidden.txt (communs à tous les projets) et ~/.config/<project>/forbidden.txt,
//   une expression régulière par ligne. Sortie 1 (2 pour claude-hook) au moindre motif trouvé.
import { execFileSync } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const git = (args: string[]) => execFileSync("git", args, { encoding: "utf8", maxBuffer: 1 << 28 });
const TOP = (() => {
  try {
    return git(["rev-parse", "--show-toplevel"]).trim();
  } catch {
    return process.cwd();
  }
})();
const CONF: { project?: string; publicRoot?: string; allow?: string[] } = (() => {
  try {
    return JSON.parse(readFileSync(join(TOP, ".public-guard.json"), "utf8"));
  } catch {
    return {};
  }
})();
const ZERO = /^0+$/;

type Rule = { name: string; re: RegExp; local?: boolean };
const GENERIC: Rule[] = [
  { name: "domaine de tailnet", re: /\b[a-z0-9-]+\.[a-z0-9-]+\.ts\.net\b/i },
  { name: "chemin personnel", re: /\/(Users|home)\/(?!linuxbrew\b|runner\b|USER\b|user\b|me\b|toi\b|you\b)[a-z][a-z0-9_.-]*\//i },
  {
    name: "adresse IP privée",
    re: /\b(10\.(?!0\.0\.1\b)\d{1,3}\.\d{1,3}\.\d{1,3}|192\.168\.(?!(0|1)\.1\b)\d{1,3}\.\d{1,3}|172\.(1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}|100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d{1,3}\.\d{1,3})\b/,
  },
  {
    name: "adresse e-mail",
    re: /\b[\w.+-]+@(?!(?:[\w-]+\.)*(?:example\.(?:com|org|net)|invalid|test|users\.noreply\.github\.com|anthropic\.com|github\.com)\b)[\w-]+(?:\.[\w-]+)*\.[a-z]{2,}\b/i,
  },
];
const FILES = [
  process.env.PUBLIC_GUARD_FORBIDDEN,
  join(homedir(), ".config", "public-guard", "forbidden.txt"),
  CONF.project ? join(homedir(), ".config", CONF.project, "forbidden.txt") : undefined,
].filter((f): f is string => !!f);
function rules(): Rule[] {
  const local = FILES.filter(existsSync).flatMap((f) =>
    readFileSync(f, "utf8")
      .split("\n")
      .map((l) => l.trim())
      .filter((l) => l && !l.startsWith("#"))
      .map((l) => ({ name: "motif perso", re: new RegExp(l), local: true }))
  );
  return [...GENERIC, ...local];
}
// Exception visible dans le diff, pour un motif générique seulement (jamais pour un motif perso).
const ALLOW = "public-guard: ok";

type Hit = { where: string; rule: string; line: string };
// Chaînes déjà publiques par décision (ex. identifiant d'application), retirées avant le scan : listées dans
// .public-guard.json, donc visibles et relues dans chaque PR.
const ALLOWED = (CONF.allow ?? []).map((a) => new RegExp(a, "g"));
export function scan(lines: { where: string; text: string }[], rs = rules()): Hit[] {
  const hits: Hit[] = [];
  for (const { where, text: raw } of lines) {
    const text = ALLOWED.reduce((t, re) => t.replace(re, ""), raw);
    for (const r of rs) if (r.re.test(text) && (r.local || !text.includes(ALLOW))) hits.push({ where, rule: r.name, line: raw.trim().slice(0, 120) });
  }
  return hits;
}

// Lignes ajoutées d'un diff unifié (git diff ou git show), avec fichier et numéro de ligne.
function addedLines(diff: string, prefix = ""): { where: string; text: string }[] {
  const out: { where: string; text: string }[] = [];
  let file = "",
    n = 0;
  for (const l of diff.split("\n")) {
    if (l.startsWith("+++ ")) file = l.replace(/^\+\+\+ (b\/)?/, "");
    else if (l.startsWith("@@")) n = Number(/\+(\d+)/.exec(l)?.[1] ?? 0);
    else if (l.startsWith("+")) out.push({ where: `${prefix}${file}:${n++}`, text: l.slice(1) });
    else if (!l.startsWith("-")) n++;
  }
  return out;
}

export function checkPush(remote: string, input: string): string[] {
  const errors: string[] = [];
  const lines: { where: string; text: string }[] = [];
  for (const row of input.split("\n").filter(Boolean)) {
    const [localRef, localSha, , remoteSha] = row.split(" ");
    if (!localSha || ZERO.test(localSha)) continue; // suppression d'une branche distante
    const commit = git(["rev-parse", `${localSha}^{commit}`]).trim();
    if (CONF.publicRoot)
      try {
        execFileSync("git", ["merge-base", "--is-ancestor", CONF.publicRoot, commit], { stdio: "ignore" });
      } catch {
        errors.push(`${localRef} ne descend pas de la racine publique ${CONF.publicRoot.slice(0, 7)} : la pousser publierait une histoire privée`);
        continue;
      }
    const range = remoteSha && !ZERO.test(remoteSha) ? [commit, `^${remoteSha}`] : [commit, "--not", `--remotes=${remote}`];
    for (const c of git(["rev-list", ...range])
      .split("\n")
      .filter(Boolean)) {
      const short = c.slice(0, 7);
      git(["log", "-1", "--format=%an <%ae>%n%cn <%ce>%n%B", c])
        .split("\n")
        .forEach((text, i) => lines.push({ where: `${short} message:${i + 1}`, text }));
      lines.push(...addedLines(git(["show", "--format=", "--no-color", "-U0", "--no-ext-diff", c]), `${short} `));
    }
  }
  return [...errors, ...scan(lines).map(fmt)];
}

const fmt = (h: Hit) => `${h.where} (${h.rule}) ${h.line}`;
const textLines = (s: string, where: string) => s.split("\n").map((text, i) => ({ where: `${where}:${i + 1}`, text }));

// Journal JSONL local (hors dépôt) : chaque refus et chaque erreur de la garde.
function log(ok: boolean, cause: string) {
  try {
    const dir = join(homedir(), ".local", "state", "public-guard");
    mkdirSync(dir, { recursive: true });
    appendFileSync(
      join(dir, "actions.jsonl"),
      JSON.stringify({ ts: new Date().toISOString(), project: CONF.project ?? TOP, action: "public_guard", ok, cause }) + "\n"
    );
  } catch {}
}

function report(problems: string[], what: string, code = 1): never {
  if (!problems.length) process.exit(0);
  console.error(`REFUSÉ (dépôt public) : ${what}`);
  for (const p of problems.slice(0, 30)) console.error("  " + p);
  if (problems.length > 30) console.error(`  ... ${problems.length - 30} de plus`);
  console.error(`Retire la donnée privée (réglage dans ~/.config/${CONF.project ?? "<projet>"}/, données de test fictives), ne contourne pas la garde.`);
  log(false, `${what} : ${problems.length} problème(s)`);
  process.exit(code);
}

if (import.meta.main) {
  const [cmd, arg] = process.argv.slice(2);
  const stdin = () => readFileSync(0, "utf8");
  if (cmd === "staged") {
    const diff = git(["diff", "--cached", "--no-color", "-U0", "--no-ext-diff", "--diff-filter=ACMR"]);
    const names = git(["diff", "--cached", "--name-only", "--diff-filter=ACR"]).split("\n").filter(Boolean);
    report(scan([...addedLines(diff), ...names.map((n) => ({ where: "nom de fichier", text: n }))]).map(fmt), "lignes ajoutées");
  } else if (cmd === "msg") {
    const text = readFileSync(arg, "utf8")
      .split("\n")
      .filter((l) => !l.startsWith("#"))
      .join("\n");
    report(scan(textLines(text, "message")).map(fmt), "message de commit");
  } else if (cmd === "push") report(checkPush(arg || "origin", stdin()), "commits poussés");
  else if (cmd === "tree") {
    const lines = git(["ls-files"])
      .split("\n")
      .filter(Boolean)
      .flatMap((f) => {
        const p = join(TOP, f);
        if (!existsSync(p)) return [];
        const buf = readFileSync(p);
        return buf.subarray(0, 8000).includes(0) ? [] : textLines(buf.toString("utf8"), f);
      });
    report(scan(lines).map(fmt), "fichiers suivis");
  } else if (cmd === "text") report(scan(textLines(stdin(), "texte")).map(fmt), "texte");
  else if (cmd === "claude-hook") {
    // Claude Code passe {tool_input: {command}} ; seules les commandes qui publient du texte sur GitHub sont lues.
    let command = "";
    try {
      command = JSON.parse(stdin()).tool_input?.command ?? "";
    } catch {}
    if (!/\bgh\s+(pr|issue)\s+(create|edit|comment|review)\b|\bgh\s+api\b.*\b(issues|pulls|comments)\b/.test(command)) process.exit(0);
    const lines = textLines(command, "commande");
    for (const m of command.matchAll(/--body-file[= ]+("([^"]+)"|'([^']+)'|(\S+))/g)) {
      const f = m[2] || m[3] || m[4];
      if (f && f !== "-" && existsSync(f)) lines.push(...textLines(readFileSync(f, "utf8"), f));
    }
    report(scan(lines).map(fmt), "texte publié sur GitHub (PR, issue, commentaire)", 2);
  } else {
    console.error("usage : bun scripts/public-guard.ts staged | msg FICHIER | push REMOTE | tree | text | claude-hook");
    process.exit(2);
  }
}
