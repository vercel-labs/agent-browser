#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTANCE_FILE="$SCRIPT_DIR/.instance"

if [[ ! -f "$INSTANCE_FILE" ]]; then
  echo "Error: No instance provisioned. Run ./scripts/windows-debug/provision.sh first."
  exit 1
fi

source "$INSTANCE_FILE"
export AWS_DEFAULT_REGION="$REGION"

STATE=$(aws ec2 describe-instances \
  --instance-ids "$INSTANCE_ID" \
  --query "Reservations[0].Instances[0].State.Name" --output text)

if [[ "$STATE" == "running" ]]; then
  echo "Instance $INSTANCE_ID is already running."
else
  echo "Starting instance $INSTANCE_ID..."
  aws ec2 start-instances --instance-ids "$INSTANCE_ID" --no-cli-pager
  echo "Waiting for running state..."
  aws ec2 wait instance-running --instance-ids "$INSTANCE_ID"
  echo "Instance is running."
fi

echo "Waiting for SSM agent connectivity..."
for i in $(seq 1 30); do
  SSM_STATUS=$(aws ssm describe-instance-information \
    --filters "Key=InstanceIds,Values=$INSTANCE_ID" \
    --query "InstanceInformationList[0].PingStatus" --output text 2>/dev/null || echo "None")
  if [[ "$SSM_STATUS" == "Online" ]]; then
    echo "SSM agent is online. Ready for commands."
    echo "  ./scripts/windows-debug/run.sh \"your-command-here\""
    exit 0
  fi
  sleep 10
done

echo "Warning: SSM agent not online after 5 minutes. The instance may still be booting."
echo "Try again in a minute: ./scripts/windows-debug/run.sh \"hostname\""
