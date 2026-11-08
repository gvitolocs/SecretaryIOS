#!/bin/bash
# Installs the COM3D2 fitting room (deploy/lucina-com3d2.service) on nezopt from this checkout:
# the release binary, the stage bundle it serves, the unit; keeps the previous binary as .bak-<ts>.
# Run from the repo root after `cargo build --release -p secretary-avatar-com3d2 --bin com3d2-native`
# and `npm run build` in packages/lucina-stage. The gateway key must already exist in
# ~/.config/lucina-com3d2/gateway.key (and the same key in p1's /etc/caddy/com3d2-gateway.key).
set -euo pipefail
ts=$(date -u +%Y%m%dT%H%M%SZ)
bin=target/release/com3d2-native
bundle=packages/lucina-stage/dist/lucina-stage.standalone.js
test -x "$bin" && test -f "$bundle" || { echo "build the binary and the stage bundle first"; exit 1; }
test -s ~/.config/lucina-com3d2/gateway.key || { echo "no gateway key"; exit 1; }
mkdir -p ~/.local/bin ~/.local/share/lucina-com3d2 ~/.config/systemd/user
[ -f ~/.local/bin/com3d2-native ] && cp -a ~/.local/bin/com3d2-native ~/.local/bin/com3d2-native.bak-"$ts"
install -m 0755 "$bin" ~/.local/bin/com3d2-native.new && mv ~/.local/bin/com3d2-native.new ~/.local/bin/com3d2-native
install -m 0644 "$bundle" ~/.local/share/lucina-com3d2/stage.js
install -m 0644 deploy/lucina-com3d2.service ~/.config/systemd/user/lucina-com3d2.service
systemctl --user daemon-reload
systemctl --user enable -q lucina-com3d2
systemctl --user restart lucina-com3d2
sleep 3
systemctl --user is-active -q lucina-com3d2 || { journalctl --user -u lucina-com3d2 -n 20 --no-pager; exit 1; }
# Without the key the service refuses; with it, the page answers.
code=$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: com3d2.internal' http://100.107.164.33:8885/)
[ "$code" = 403 ] || { echo "expected 403 without the key, got $code"; exit 1; }
echo "lucina-com3d2 running (backup ~/.local/bin/com3d2-native.bak-$ts if there was one)"
