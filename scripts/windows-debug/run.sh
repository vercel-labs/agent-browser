#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTANCE_FILE="$SCRIPT_DIR/.instance"

if [[ ! -f "$INSTANCE_FILE" ]]; then
  echo "Error: No instance provisioned. Run ./scripts/windows-debug/provision.sh first."
  exit 1
fi

if [[ $# -eq 0 ]]; then
  echo "Usage: ./scripts/windows-debug/run.sh \"<powershell-command>\""
  echo ""
  echo "Examples:"
  echo "  ./scripts/windows-debug/run.sh \"cd C:\\agent-browser && cargo test\""
  echo "  ./scripts/windows-debug/run.sh \"Get-Content C:\\bootstrap.log\""
  echo "  ./scripts/windows-debug/run.sh \"cd C:\\agent-browser && cargo test e2e -- --ignored --test-threads=1\""
  exit 1
fi

source "$INSTANCE_FILE"
export AWS_DEFAULT_REGION="$REGION"

COMMAND="$*"

PARAMS_FILE=$(mktemp)
trap "rm -f $PARAMS_FILE" EXIT

python3 -c '
import json, sys
path_setup = "$env:PATH = \"$env:USERPROFILE\\.cargo\\bin;C:\\Program Files\\Git\\cmd;$env:PATH\""
cmd = path_setup + "\n" + sys.argv[1]
json.dump({"commands": [cmd]}, open(sys.argv[2], "w"))
' "$COMMAND" "$PARAMS_FILE"

COMMAND_ID=$(aws ssm send-command \
  --instance-ids "$INSTANCE_ID" \
  --document-name "AWS-RunPowerShellScript" \
  --parameters "file://$PARAMS_FILE" \
  --timeout-seconds 3600 \
  --query "Command.CommandId" --output text)

echo "Command sent (ID: $COMMAND_ID). Waiting..." >&2

while true; do
  RESULT=$(aws ssm get-command-invocation \
    --command-id "$COMMAND_ID" \
    --instance-id "$INSTANCE_ID" \
    --output json 2>&1) || true

  STATUS=$(echo "$RESULT" | python3 -c "
import sys, json
try:
    print(json.loads(sys.stdin.read()).get('Status', 'Unknown'))
except:
    print('Pending')
" 2>/dev/null)

  case "$STATUS" in
    Success)
      echo "$RESULT" | python3 -c "
import sys, json
r = json.loads(sys.stdin.read())
out = r.get('StandardOutputContent', '').rstrip()
err = r.get('StandardErrorContent', '').rstrip()
if out:
    print(out)
if err:
    print(err, file=sys.stderr)
"
      exit 0
      ;;
    Failed|TimedOut|Cancelled)
      echo "$RESULT" | python3 -c "
import sys, json
r = json.loads(sys.stdin.read())
out = r.get('StandardOutputContent', '').rstrip()
err = r.get('StandardErrorContent', '').rstrip()
if out:
    print(out)
if err:
    print(err, file=sys.stderr)
"
      echo "Command $STATUS." >&2
      exit 1
      ;;
    *)
      sleep 3
      ;;
  esac
done
