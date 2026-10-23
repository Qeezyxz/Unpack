#!/bin/bash
# Runs the real src/app.js against a mock Tauri bridge in headless Chrome and
# prints PASS/FAIL per scenario. Needs python3 and Chrome (set CHROME to override).
set -u
here="$(cd "$(dirname "$0")" && pwd)"
src="$here/../../src"
chrome="${CHROME:-$([ -x /opt/google/chrome/chrome ] && echo /opt/google/chrome/chrome || command -v google-chrome || command -v chromium)}"
work="$(mktemp -d)"
port=$((20000 + RANDOM % 20000))
cp "$src"/app.js "$src"/styles.css "$src"/theme.js "$src"/logo.png "$work"/
cp "$here"/mock.js "$here"/scenarios.js "$work"/
sed 's#<script src="app.js"></script>#<script src="mock.js"></script><script src="app.js"></script><script src="scenarios.js"></script>#' "$src/index.html" > "$work/index.html"
(cd "$work" && python3 -m http.server "$port" >/dev/null 2>&1) &
server=$!
trap 'kill $server 2>/dev/null; rm -rf "$work"' EXIT
sleep 1
out="$(timeout 90 "$chrome" --headless=new --no-sandbox --disable-gpu --disable-dev-shm-usage \
  --user-data-dir="$work/profile" --virtual-time-budget=20000 --dump-dom "http://127.0.0.1:$port/index.html" 2>/dev/null |
  python3 -c "
import sys, re, html
dom = sys.stdin.read()
m = re.search(r'<pre id=\"results\">(.*?)</pre>', dom, re.S)
print(html.unescape(m.group(1)) if m else 'NO RESULTS')")"
echo "$out"
if echo "$out" | grep -q "^FAIL\|NO RESULTS\|^CRASH"; then exit 1; fi
