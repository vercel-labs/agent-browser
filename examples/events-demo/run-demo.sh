#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"
AB="${AB_BIN:-agent-browser}"
MODE="${1:-}"
PIDS=""

cleanup() {
  trap - EXIT INT TERM
  "$AB" dashboard stop >/dev/null 2>&1
  for pid in $PIDS; do
    kill "$pid" 2>/dev/null
  done
}
trap cleanup EXIT
trap 'exit 130' INT TERM

if ! "$AB" events --help >/dev/null 2>&1; then
  echo "This agent-browser binary has no events command. Build or install a binary that includes it." >&2
  exit 1
fi

case "$MODE" in
  --live)
    AI_GATEWAY_API_KEY="$(security find-generic-password -s 'Vercel AI Gateway' -a vercel-desktop-key -w)" || exit 1; export AI_GATEWAY_API_KEY
    ;;
  --fake)
    python3 "$DIR/fake-gateway.py" &
    PIDS="$PIDS $!"
    AI_GATEWAY_URL=http://127.0.0.1:4950
    AI_GATEWAY_API_KEY=fake
    export AI_GATEWAY_URL AI_GATEWAY_API_KEY
    ;;
  "")
    if [ -z "${AI_GATEWAY_API_KEY:-}" ]; then
      echo "AI_GATEWAY_API_KEY is not set. Chat will be disabled; use --live or --fake." >&2
    fi
    ;;
  *)
    echo "usage: $0 [--live | --fake]" >&2
    exit 2
    ;;
esac

"$DIR/serve.sh" >/dev/null 2>&1 &
PIDS="$PIDS $!"

"$AB" dashboard stop >/dev/null 2>&1
"$AB" dashboard start || exit 1

echo
echo "Form page:  http://127.0.0.1:4900/index.html"
echo "Dashboard:  http://localhost:4848"
echo "Package:    $DIR/deployment-package.zip"
echo
echo "Paste this into the dashboard chat:"
echo "Open http://127.0.0.1:4900, fill the deployment form with project name Acme Dashboard, environment Production, attach $DIR/deployment-package.zip, and deploy it. Do not poll and do not block: subscribe to whatever tells you the upload has finished, submit, then stop. Tell me when the event wakes you."
echo
echo "Press Ctrl-C to stop everything this script started."

while :; do
  sleep 1
done
