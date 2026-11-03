# Instructions for AI agents

You are an AI coding agent (Claude Code, Codex, Cursor...) and your user asked you to install or work on **tv-home-webos**. This file tells you how. Keep the safety rules at the end: they matter more than the rest.

## What it does

An Apple TV-style home screen for LG webOS TVs, in two parts:

- `app/`: the webOS app, installed on the TV in developer mode.
- `companion/`: a Rust service (tokio + axum) on an always-on machine of the local network. It opens the app when the TV turns on (LG SSAP network API, optional home button redirect), remembers the last channel and opened apps, serves the French DTT guide (xmltvfr.fr), TMDB-based suggestions from the user's ratings, and a phone page on port 8787 (`TVHOME_PORT`) to rate titles and drive the TV.

Tested on an LG 43UP80006LR (webOS 6). Unofficial, not affiliated with LG.

## Install

Run, then read the output: every line starts with `✓`, `!` (optional piece missing or a step left to the user) or `✗` (blocking).

```sh
curl -fsSL https://raw.githubusercontent.com/pyxl-dev/tv-home-webos/main/install.sh | sh
```

It clones to `~/.local/share/tv-home-webos`, creates `app/config.js` and `companion/tv-home.env` from their templates (never overwriting), builds the companion with `cargo`, packages the app with `ares-package` and, on Linux with systemd, writes (but does not enable) the user unit. It never talks to the TV.

- `! cargo missing` / `! ares-cli not found`: tell the user and give the command printed. Do not install system tools without asking.
- The user must provide, do not invent them: the TV IP (`TVHOME_TV_IP`), optionally its MAC (`TVHOME_TV_MAC`), the companion address in `app/config.js` (`window.SERVER`), and a TMDB key (`companion/data/settings.json`, `{"tmdbKey": "..."}`). The example values (`192.168.1.50`, `aa:bb:cc:dd:ee:ff`, `192.168.1.10`) are placeholders.
- Installing the app on the TV (`ares-install -d <device> dist/*.ipk`) needs a TV in developer mode and an `ares-cli` device. Only do it when the user asks.
- First start: the TV shows a pairing prompt. The user accepts it with the real remote.
- Uninstall: `./install.sh --uninstall` (keeps a folder that holds the user's config, pairing key or ratings).

## Use

- Companion: `systemctl --user enable --now tv-home-companion`, logs in `companion/companion.log`. It refuses to start without `TVHOME_TV_IP`. `TVHOME_NO_TV=1` runs it without a TV (testing, with `TVHOME_PORT` and a copy of `data/`).
- Phone page: `http://<server-ip>:8787`.
- `node tools/tv.mjs` (Node 22+): screenshot of the TV, app screenshot, JS eval in the app, remote keys, click a button by name, turn the TV off. Run it without argument for help. Careful with `keys`: OK acts on whatever has focus.
- Develop: `cd companion && cargo build --release`, run it from the `companion` directory (it reads `data/`, `public/`, `manifest.json`, `client-key.txt` there).

## Safety rules

- Never commit personal files: `app/config.js`, `companion/tv-home.env`, `companion/client-key.txt` (pairing key), `companion/data/` (TMDB key, ratings, cache). They are in `.gitignore`.
- Never put a real IP, MAC, key or token in a README, a commit or an issue. Use placeholders.
- Do not act on the user's real TV (install, turn off, send keys, pair) unless they asked for it in this session.
- The companion has no authentication: keep it on the local network, never expose its port to the Internet.
