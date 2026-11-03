#!/bin/sh
# tv-home-webos installer.
#
#   curl -fsSL https://raw.githubusercontent.com/pyxl-dev/tv-home-webos/main/install.sh | sh
#   ./install.sh               # from a clone: installs that clone
#   ./install.sh --uninstall
#
# Gets the code in ~/.local/share/tv-home-webos (or uses this clone), creates
# your two local config files from their templates, builds the Rust companion,
# packages the TV app and writes a systemd user unit (Linux). It never touches
# the TV and never enables the service: that stays your call once the config is
# filled in. Re-run it to update. Nothing outside your home folder is touched.
set -eu

REPO="${TVHOME_REPO:-https://github.com/pyxl-dev/tv-home-webos.git}"
DEST="${TVHOME_HOME:-$HOME/.local/share/tv-home-webos}"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/tv-home-companion.service"

if [ -t 1 ]; then B=$(printf '\033[1m'); G=$(printf '\033[32m'); Y=$(printf '\033[33m'); R=$(printf '\033[31m'); N=$(printf '\033[0m'); else B= G= Y= R= N=; fi
ok()   { printf '%s✓%s %s\n' "$G" "$N" "$1"; }
warn() { printf '%s!%s %s\n' "$Y" "$N" "$1"; }
die()  { printf '%s✗%s %s\n' "$R" "$N" "$1" >&2; exit 1; }

if [ "${1:-}" = "--uninstall" ]; then
  if command -v systemctl >/dev/null 2>&1 && [ -f "$UNIT" ]; then
    systemctl --user disable --now tv-home-companion >/dev/null 2>&1 </dev/null || true
  fi
  if [ -f "$UNIT" ]; then rm -f "$UNIT"; ok "removed $UNIT"; fi
  if [ -d "$DEST/.git" ]; then
    rm -rf "$DEST/companion/target" "$DEST/dist"
    if [ -e "$DEST/companion/tv-home.env" ] || [ -e "$DEST/companion/client-key.txt" ] || [ -d "$DEST/companion/data" ] || [ -e "$DEST/app/config.js" ]; then
      warn "kept $DEST: it holds your config, TV pairing key and ratings. Delete it yourself if you do not need them:  rm -rf $DEST"
    else
      rm -rf "$DEST"; ok "removed $DEST"
    fi
  fi
  echo "The TV app stays on the TV until you remove it:  ares-install -d <device> --remove <app id from app/appinfo.json>"
  exit 0
fi

printf '%stv-home-webos%s installer\n\n' "$B" "$N"

# 1. Code: this clone, or a managed one in ~/.local/share.
HERE=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)
if [ -n "$HERE" ] && [ -d "$HERE/companion" ] && [ -d "$HERE/app" ]; then
  DEST="$HERE"
  ok "using this clone: $DEST"
elif [ -d "$DEST/.git" ]; then
  git -C "$DEST" pull --ff-only -q </dev/null || die "could not update $DEST (local changes?)"
  ok "updated $DEST"
else
  command -v git >/dev/null || die "git is missing: install it, then re-run."
  mkdir -p "$(dirname "$DEST")"
  git clone -q --depth 1 "$REPO" "$DEST" </dev/null || die "could not download $REPO"
  ok "downloaded to $DEST"
fi

# 2. Local config files, created from the templates and never overwritten.
if [ -f "$DEST/app/config.js" ]; then ok "app/config.js already there"
else cp "$DEST/app/config.example.js" "$DEST/app/config.js"; warn "created app/config.js: set window.SERVER to your server address"; fi
if [ -f "$DEST/companion/tv-home.env" ]; then ok "companion/tv-home.env already there"
else cp "$DEST/companion/tv-home.env.example" "$DEST/companion/tv-home.env"; warn "created companion/tv-home.env: set TVHOME_TV_IP (and TVHOME_TV_MAC)"; fi

# 3. Companion (Rust).
if command -v cargo >/dev/null 2>&1; then
  if (cd "$DEST/companion" && cargo build --release -q >/dev/null 2>&1 </dev/null); then ok "built companion/target/release/tv-home-companion"
  else warn "companion build failed: run  cd $DEST/companion && cargo build --release  to see why."; fi
else
  warn "cargo missing: install Rust (https://rustup.rs) to build the companion."
fi

# 4. TV app package (needs the LG webOS CLI).
if command -v ares-package >/dev/null 2>&1; then
  mkdir -p "$DEST/dist"
  if (cd "$DEST" && ares-package app -o dist >/dev/null 2>&1 </dev/null); then ok "packaged the TV app in $DEST/dist"
  else warn "ares-package failed: run  cd $DEST && ares-package app -o dist  to see why."; fi
else
  warn "ares-cli not found: install it (npm install -g @webos-tools/cli) to package the TV app."
fi

# 5. systemd user unit (Linux): written, not enabled.
if [ "$(uname -s)" = "Linux" ] && command -v systemctl >/dev/null 2>&1; then
  mkdir -p "$UNIT_DIR"
  sed "s#%h/tv-home#$DEST#g" "$DEST/companion/tv-home-companion.service" > "$UNIT"
  systemctl --user daemon-reload >/dev/null 2>&1 </dev/null || true
  ok "wrote $UNIT (not enabled yet)"
else
  warn "no systemd here: run the companion yourself from $DEST/companion (see the README)."
fi

printf '\n%sNext steps.%s\n' "$B" "$N"
echo "  1. Fill in $DEST/app/config.js and $DEST/companion/tv-home.env"
echo "  2. Install the app on the TV:  cd $DEST && ares-install -d <device> dist/*.ipk"
echo "  3. Start the companion (Linux with systemd):  systemctl --user enable --now tv-home-companion"
echo "  4. At first start, accept the pairing prompt on the TV with the real remote."
