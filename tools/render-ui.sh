#!/usr/bin/env bash
#
# Renders FlatBak's pages to PNG files inside a headless Weston, so the UI can
# be checked without a desktop session. Requires `weston` and a build of the
# `render_ui` example (`cargo build --examples`).
#
#   tools/render-ui.sh [output-directory]
#
set -e
OUT="${1:-/tmp/flatbak-shots}"
PROJECT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export XDG_RUNTIME_DIR=/tmp/wl-run
mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
pkill -x weston 2>/dev/null || true
sleep 1

weston --backend=headless --width=940 --height=860 --socket=fbrender \
       --shell=kiosk-shell.so >/tmp/weston-render.log 2>&1 &
WESTON_PID=$!
for _ in $(seq 40); do [ -S "$XDG_RUNTIME_DIR/fbrender" ] && break; sleep 0.25; done

# A private config home, so the harness can pick a theme without touching the
# user's own GTK settings. Weston has no settings portal, so without this GTK
# falls back to its built-in icons only.
export XDG_CONFIG_HOME=/tmp/fb-config
mkdir -p "$XDG_CONFIG_HOME/gtk-4.0"
cat > "$XDG_CONFIG_HOME/gtk-4.0/settings.ini" <<INI
[Settings]
gtk-icon-theme-name=Adwaita
gtk-theme-name=Adwaita
INI

export WAYLAND_DISPLAY=fbrender
unset DISPLAY
export GDK_BACKEND=wayland
export GSK_RENDERER=cairo

cd "$PROJECT"
timeout 60 ./target/debug/examples/render_ui "$OUT" 2>&1 | tail -20

kill $WESTON_PID 2>/dev/null || true
sleep 0.5
kill -9 $WESTON_PID 2>/dev/null || true
ls -la "$OUT"
exit 0
