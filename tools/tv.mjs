#!/usr/bin/env node
// Look at and drive the TV from the Mac (testing helper; Node 22+).
//
//   node tools/tv.mjs shot out.jpg        TV screenshot (SSAP): the real screen, video and LG overlays included, 960x540
//   node tools/tv.mjs off                 turn the TV off
//   node tools/tv.mjs app-shot out.png    screenshot of the Accueil app only (DevTools), 1920x1080, no video
//   node tools/tv.mjs eval 'js'           run JS in the app (promises are awaited), print the result
//   node tools/tv.mjs keys 38,w500,13     remote keys (37-40 arrows, 13 OK, 461 Back; wN = wait N ms)
//   node tools/tv.mjs click 'Bande-annonce'   click the visible button whose text matches
//
// Careful with `keys`: OK acts on whatever has focus (a verdict button, an app tile…). Check with `eval`
// first (body.on-shelf, #sheet.open, #trailer.open) or use `click`, which only targets a named button.
// SSAP needs companion/client-key.txt (paired key); DevTools needs dev mode and ares-cli (device lg-tv).
import fs from 'fs';
import { spawn } from 'child_process';
import { fileURLToPath } from 'url';

const DIR = fileURLToPath(new URL('../companion/', import.meta.url));
// TV address: TVHOME_TV_IP from the environment, else from companion/tv-home.env (same file as the companion)
const ENV_FILE = fs.existsSync(DIR + 'tv-home.env') ? fs.readFileSync(DIR + 'tv-home.env', 'utf8') : '';
const TV = process.env.TVHOME_TV_IP || (ENV_FILE.match(/^\s*TVHOME_TV_IP\s*=\s*(\S+)/m) || [])[1];
const DEVICE = 'lg-tv', APP = 'com.yoan.home';
const [cmd, ...args] = process.argv.slice(2);
const sleep = ms => new Promise(r => setTimeout(r, ms));
setTimeout(() => { console.error('timeout'); process.exit(2); }, 120000).unref();

// ---------- SSAP (LG's network API, same pairing as the companion) ----------
async function ssap(uri, payload) {
  process.emitWarning = () => {}; // the TV's certificate is self-signed
  process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';
  const m = JSON.parse(fs.readFileSync(DIR + 'manifest.json', 'utf8'));
  const key = fs.readFileSync(DIR + 'client-key.txt', 'utf8').trim();
  if (!TV) throw new Error('TVHOME_TV_IP missing: set it or fill companion/tv-home.env');
  const ws = new WebSocket(`wss://${TV}:3001`);
  return new Promise((resolve, reject) => {
    ws.onerror = () => reject(new Error('TV unreachable (off?)'));
    ws.onopen = () => ws.send(JSON.stringify({ type: 'register', id: 'reg', payload: { pairingType: 'PROMPT', 'client-key': key, manifest: m.manifest || m } }));
    ws.onmessage = e => {
      const r = JSON.parse(e.data);
      if (r.id === 'reg' && r.type === 'registered') ws.send(JSON.stringify({ type: 'request', id: 'q', uri, payload }));
      if (r.id === 'q') { ws.close(); r.type === 'error' ? reject(new Error(r.error)) : resolve(r.payload); }
    };
  });
}

// ---------- DevTools on the app (through ares-inspect's local proxy) ----------
async function devtools() {
  const proc = spawn('ares-inspect', ['-d', DEVICE, '-a', APP], { stdio: ['ignore', 'pipe', 'pipe'] });
  const port = await new Promise((resolve, reject) => {
    let out = '';
    const on = d => { out += d; const m = out.match(/localhost:(\d+)/); if (m) resolve(m[1]); };
    proc.stdout.on('data', on); proc.stderr.on('data', on);
    proc.on('exit', () => reject(new Error('ares-inspect failed: ' + out.trim())));
  });
  let page;
  for (let i = 0; i < 20 && !page; i++) {
    try { page = (await (await fetch(`http://localhost:${port}/json`)).json()).find(t => t.type === 'page'); } catch { await sleep(300); }
  }
  if (!page) throw new Error('app page not found (is the Accueil app running?)');
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = () => j(new Error('DevTools connection failed')); });
  let id = 0; const pend = {};
  ws.onmessage = e => { const m = JSON.parse(e.data); if (pend[m.id]) pend[m.id](m); };
  const send = (method, params = {}) => new Promise((r, j) => {
    pend[++id] = m => m.error ? j(new Error(m.error.message)) : r(m.result);
    ws.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async x => {
    const r = await send('Runtime.evaluate', { expression: x, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || 'JS error');
    return r.result.value;
  };
  return { send, evaluate, close: () => { ws.close(); proc.kill(); } };
}

async function keys(dt, list) {
  for (const k of list.split(',').filter(Boolean)) {
    if (k[0] === 'w') { await sleep(+k.slice(1)); continue; }
    for (const type of ['rawKeyDown', 'keyUp']) await dt.send('Input.dispatchKeyEvent', { type, windowsVirtualKeyCode: +k, nativeVirtualKeyCode: +k });
    await sleep(450);
  }
}

try {
  if (cmd === 'shot') {
    const r = await ssap('ssap://tv/executeOneShot');
    fs.writeFileSync(args[0] || 'tv.jpg', Buffer.from(await (await fetch(r.imageUri)).arrayBuffer()));
    console.log(args[0] || 'tv.jpg');
  } else if (cmd === 'off') {
    await ssap('ssap://system/turnOff'); console.log('off');
  } else if (['app-shot', 'eval', 'keys', 'click'].includes(cmd)) {
    const dt = await devtools();
    try {
      if (cmd === 'app-shot') {
        const r = await dt.send('Page.captureScreenshot', { format: 'png' });
        fs.writeFileSync(args[0] || 'app.png', Buffer.from(r.data, 'base64')); console.log(args[0] || 'app.png');
      } else if (cmd === 'eval') console.log(JSON.stringify(await dt.evaluate(args[0])));
      else if (cmd === 'keys') await keys(dt, args[0] || '');
      else console.log(await dt.evaluate(`(function (t) {
        var b = [].filter.call(document.querySelectorAll(".btn, .chip, .tile"), function (x) { return x.offsetParent && x.textContent.indexOf(t) >= 0; })[0];
        if (!b) return "no visible button matching " + JSON.stringify(t);
        b.click(); return "clicked " + JSON.stringify(b.textContent.trim());
      })(${JSON.stringify(args[0] || '')})`));
    } finally { dt.close(); }
  } else {
    console.log(fs.readFileSync(fileURLToPath(import.meta.url), 'utf8').split('\n').filter(l => l.startsWith('//')).map(l => l.slice(3)).join('\n'));
  }
  process.exit(0);
} catch (e) { console.error(e.message); process.exit(1); }
