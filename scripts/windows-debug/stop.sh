#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTANCE_FILE="$SCRIPT_DIR/.instance"

if [[ ! -f "$INSTANCE_FILE" ]]; then
  echo "Error: No instance provisioned. Nothing to stop."
  exit 1
fi

source "$INSTANCE_FILE"
export AWS_DEFAULT_REGION="$REGION"

STATE=$(aws ec2 describe-instances \
  --instance-ids "$INSTANCE_ID" \
  --query "Reservations[0].Instances[0].State.Name" --output text)

if [[ "$STATE" == "stopped" ]]; then
  echo "Instance $INSTANCE_ID is already stopped."
  exit 0
fi

echo "Stopping instance $INSTANCE_ID..."
aws ec2 stop-instances --instance-ids "$INSTANCE_ID" --no-cli-pager
echo "Waiting for stopped state..."
aws ec2 wait instance-stopped --instance-ids "$INSTANCE_ID"
echo "Instance stopped. No compute charges while stopped (storage only: ~$0.64/mo)."
