#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTANCE_FILE="$SCRIPT_DIR/.instance"
NAME_PREFIX="agent-browser-debug"
INSTANCE_TYPE="${INSTANCE_TYPE:-t3.xlarge}"

if [[ -f "$INSTANCE_FILE" ]]; then
  echo "Error: Instance already provisioned. See $INSTANCE_FILE"
  echo "Run ./scripts/windows-debug/start.sh to start it, or delete .instance to re-provision."
  exit 1
fi

REGION=$(aws configure get region 2>/dev/null || echo "")
if [[ -z "$REGION" ]]; then
  echo "Error: No AWS region configured. Run: aws configure set region us-east-1"
  exit 1
fi

echo "Provisioning Windows debug instance in $REGION..."

# --- IAM Role for SSM ---
ROLE_NAME="${IAM_ROLE_NAME:-$NAME_PREFIX-ssm-role}"
PROFILE_NAME="${INSTANCE_PROFILE_NAME:-$NAME_PREFIX-instance-profile}"

if aws iam get-instance-profile --instance-profile-name "$PROFILE_NAME" &>/dev/null; then
  echo "Instance profile $PROFILE_NAME already exists, reusing."
else
  echo "Instance profile $PROFILE_NAME not found. Creating IAM resources..."

  if ! aws iam get-role --role-name "$ROLE_NAME" &>/dev/null; then
    echo "Creating IAM role: $ROLE_NAME"
    if ! aws iam create-role \
      --role-name "$ROLE_NAME" \
      --assume-role-policy-document '{
        "Version": "2012-10-17",
        "Statement": [{
          "Effect": "Allow",
          "Principal": {"Service": "ec2.amazonaws.com"},
          "Action": "sts:AssumeRole"
        }]
      }' \
      --no-cli-pager; then

      echo ""
      echo "Error: Failed to create IAM role (see error above)."
      echo ""
      echo "Ask an IAM admin to create the following, then re-run with:"
      echo "  INSTANCE_PROFILE_NAME=<name> ./scripts/windows-debug/provision.sh"
      echo ""
      echo "What the admin needs to create:"
      echo "  1. IAM Role: $ROLE_NAME"
      echo "     - Trusted entity: EC2 (ec2.amazonaws.com)"
      echo "     - Attached policy: AmazonSSMManagedInstanceCore"
      echo "  2. Instance Profile: $PROFILE_NAME"
      echo "     - With the above role added to it"
      echo ""
      echo "Or run these commands with an account that has iam:CreateRole permission:"
      echo ""
      echo "  aws iam create-role --role-name $ROLE_NAME \\"
      echo "    --assume-role-policy-document '{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Principal\":{\"Service\":\"ec2.amazonaws.com\"},\"Action\":\"sts:AssumeRole\"}]}'"
      echo ""
      echo "  aws iam attach-role-policy --role-name $ROLE_NAME \\"
      echo "    --policy-arn arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
      echo ""
      echo "  aws iam create-instance-profile --instance-profile-name $PROFILE_NAME"
      echo ""
      echo "  aws iam add-role-to-instance-profile \\"
      echo "    --instance-profile-name $PROFILE_NAME --role-name $ROLE_NAME"
      exit 1
    fi

    aws iam attach-role-policy \
      --role-name "$ROLE_NAME" \
      --policy-arn arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore
  else
    echo "IAM role $ROLE_NAME already exists."
  fi

  echo "Creating instance profile: $PROFILE_NAME"
  aws iam create-instance-profile --instance-profile-name "$PROFILE_NAME" --no-cli-pager
  aws iam add-role-to-instance-profile \
    --instance-profile-name "$PROFILE_NAME" \
    --role-name "$ROLE_NAME"
  echo "Waiting for instance profile propagation..."
  sleep 10
fi

# --- Security Group (no inbound rules) ---
VPC_ID=$(aws ec2 describe-vpcs --filters "Name=isDefault,Values=true" --query "Vpcs[0].VpcId" --output text)
if [[ "$VPC_ID" == "None" || -z "$VPC_ID" ]]; then
  echo "Error: No default VPC found. Create one with: aws ec2 create-default-vpc"
  exit 1
fi

SG_NAME="$NAME_PREFIX-sg"
SG_ID=$(aws ec2 describe-security-groups \
  --filters "Name=group-name,Values=$SG_NAME" "Name=vpc-id,Values=$VPC_ID" \
  --query "SecurityGroups[0].GroupId" --output text 2>/dev/null || echo "None")

if [[ "$SG_ID" == "None" || -z "$SG_ID" ]]; then
  echo "Creating security group: $SG_NAME"
  SG_ID=$(aws ec2 create-security-group \
    --group-name "$SG_NAME" \
    --description "agent-browser Windows debug instance (SSM only, no inbound)" \
    --vpc-id "$VPC_ID" \
    --query "GroupId" --output text)

  # Revoke default egress isn't needed; SSM requires outbound HTTPS.
  # No inbound rules -- SSM uses outbound connections only.
else
  echo "Security group $SG_NAME ($SG_ID) already exists, reusing."
fi

# --- AMI (latest Windows Server 2022) ---
AMI_ID=$(aws ssm get-parameter \
  --name "/aws/service/ami-windows-latest/Windows_Server-2022-English-Full-Base" \
  --query "Parameter.Value" --output text)
echo "Using AMI: $AMI_ID (Windows Server 2022)"

# --- UserData bootstrap script ---
USERDATA_FILE=$(mktemp)
trap "rm -f $USERDATA_FILE" EXIT

cat > "$USERDATA_FILE" <<'PWSH'
<powershell>
$ErrorActionPreference = "Continue"
$logFile = "C:\bootstrap.log"

function Log($msg) {
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    "$ts  $msg" | Tee-Object -FilePath $logFile -Append
}

Log "--- Bootstrap starting ---"

# Install Git
Log "Installing Git..."
$gitInstaller = "$env:TEMP\git-installer.exe"
Invoke-WebRequest -Uri "https://github.com/git-for-windows/git/releases/download/v2.47.1.windows.2/Git-2.47.1.2-64-bit.exe" -OutFile $gitInstaller
Start-Process -FilePath $gitInstaller -ArgumentList "/VERYSILENT /NORESTART /NOCANCEL /SP- /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS /COMPONENTS=`"icons,ext\reg\shellhere,assoc,assoc_sh`"" -Wait
$env:PATH = "C:\Program Files\Git\cmd;$env:PATH"
[Environment]::SetEnvironmentVariable("PATH", "C:\Program Files\Git\cmd;$([Environment]::GetEnvironmentVariable('PATH', 'Machine'))", "Machine")
Log "Git installed: $(git --version)"

# Install Rust
Log "Installing Rust..."
$rustupInit = "$env:TEMP\rustup-init.exe"
Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustupInit
Start-Process -FilePath $rustupInit -ArgumentList "-y --default-toolchain stable" -Wait
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
[Environment]::SetEnvironmentVariable("PATH", "$env:USERPROFILE\.cargo\bin;$([Environment]::GetEnvironmentVariable('PATH', 'Machine'))", "Machine")
Log "Rust installed: $(rustc --version)"

# Install MSVC build tools (required for Rust on Windows)
Log "Installing Visual Studio Build Tools..."
$vsInstaller = "$env:TEMP\vs_buildtools.exe"
Invoke-WebRequest -Uri "https://aka.ms/vs/17/release/vs_buildtools.exe" -OutFile $vsInstaller
Start-Process -FilePath $vsInstaller -ArgumentList "--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" -Wait
Log "Build tools installed."

# Clone repo
Log "Cloning agent-browser..."
git clone https://github.com/vercel-labs/agent-browser.git C:\agent-browser
Set-Location C:\agent-browser
Log "Repo cloned."

# Build CLI
Log "Building agent-browser CLI..."
cargo build --release --manifest-path cli\Cargo.toml
Log "Build complete."

# Install Chrome
Log "Installing Chrome via agent-browser..."
.\cli\target\release\agent-browser.exe install
Log "Chrome installed."

Log "--- Bootstrap complete ---"
</powershell>
PWSH

# --- Launch instance ---
echo "Launching $INSTANCE_TYPE instance..."
INSTANCE_ID=$(aws ec2 run-instances \
  --image-id "$AMI_ID" \
  --instance-type "$INSTANCE_TYPE" \
  --iam-instance-profile "Name=$PROFILE_NAME" \
  --security-group-ids "$SG_ID" \
  --user-data "file://$USERDATA_FILE" \
  --block-device-mappings '[{"DeviceName":"/dev/sda1","Ebs":{"VolumeSize":80,"VolumeType":"gp3"}}]' \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$NAME_PREFIX}]" \
  --metadata-options "HttpTokens=required" \
  --query "Instances[0].InstanceId" --output text)

echo "Instance launched: $INSTANCE_ID"

# Save instance config
cat > "$INSTANCE_FILE" <<EOF
INSTANCE_ID=$INSTANCE_ID
REGION=$REGION
EOF

echo "Waiting for instance to enter running state..."
aws ec2 wait instance-running --instance-ids "$INSTANCE_ID"
echo "Instance is running."

echo ""
echo "Instance $INSTANCE_ID is booting and bootstrapping (Rust, Git, Chrome)."
echo "Bootstrap takes ~15-20 minutes on first boot."
echo ""
echo "Check bootstrap progress:"
echo "  ./scripts/windows-debug/run.sh \"Get-Content C:\\bootstrap.log\""
echo ""
echo "Once ready, sync your branch and start debugging:"
echo "  ./scripts/windows-debug/sync.sh"
echo "  ./scripts/windows-debug/run.sh \"cd C:\\agent-browser && cargo test\""
echo ""
echo "Stop when done to save costs:"
echo "  ./scripts/windows-debug/stop.sh"
