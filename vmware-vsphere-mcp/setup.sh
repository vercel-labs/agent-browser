#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLAUDE_CONFIG_DIR=""

# Detect OS for Claude Desktop config path
case "$(uname -s)" in
  Darwin)
    CLAUDE_CONFIG_DIR="$HOME/Library/Application Support/Claude"
    ;;
  Linux)
    CLAUDE_CONFIG_DIR="$HOME/.config/Claude"
    ;;
  MINGW*|CYGWIN*|MSYS*)
    CLAUDE_CONFIG_DIR="$APPDATA/Claude"
    ;;
  *)
    echo "Unsupported OS. Please configure Claude Desktop manually."
    exit 1
    ;;
esac

CLAUDE_CONFIG_FILE="$CLAUDE_CONFIG_DIR/claude_desktop_config.json"

# Step 1: Create .env from template if not exists
if [ ! -f "$SCRIPT_DIR/.env" ]; then
  echo "Creating .env from .env.example ..."
  cp "$SCRIPT_DIR/.env.example" "$SCRIPT_DIR/.env"
  echo ""
  echo "Edit $SCRIPT_DIR/.env and fill in your vCenter credentials, then re-run this script."
  exit 0
fi

# Step 2: Start the VMware vSphere MCP server via Docker Compose
echo "Starting VMware vSphere MCP server ..."
docker compose -f "$SCRIPT_DIR/docker-compose.yml" up -d --build

echo "Waiting for server to be ready ..."
sleep 5

if ! curl -sf http://localhost:8000/health > /dev/null 2>&1; then
  echo "Warning: server health check failed, but continuing setup."
fi

# Step 3: Update Claude Desktop config
# Claude Desktop requires stdio-based MCP servers. We use supergateway (via npx)
# to proxy the HTTP MCP endpoint to stdio.
mkdir -p "$CLAUDE_CONFIG_DIR"

if [ -f "$CLAUDE_CONFIG_FILE" ]; then
  export CLAUDE_CONFIG_FILE
  python3 - <<'PYEOF'
import json, os

config_file = os.environ["CLAUDE_CONFIG_FILE"]
with open(config_file, "r") as f:
    config = json.load(f)

config.setdefault("mcpServers", {})["vsphere"] = {
    "command": "npx",
    "args": ["-y", "supergateway", "--streamableHttp", "http://localhost:8000/mcp"]
}

with open(config_file, "w") as f:
    json.dump(config, f, indent=2)

print(f"Updated {config_file}")
PYEOF
else
  cat > "$CLAUDE_CONFIG_FILE" <<'EOF'
{
  "mcpServers": {
    "vsphere": {
      "command": "npx",
      "args": ["-y", "supergateway", "--streamableHttp", "http://localhost:8000/mcp"]
    }
  }
}
EOF
  echo "Created $CLAUDE_CONFIG_FILE"
fi

echo ""
echo "Setup complete. Restart Claude Desktop to load the VMware vSphere MCP server."
echo ""
echo "Available tools include:"
echo "  - List VMs with power states"
echo "  - Get VM details and performance metrics"
echo "  - Power on/off/reset VMs"
echo "  - Manage snapshots"
echo "  - Monitor hosts, datastores, networks"
echo ""
echo "To stop the MCP server:"
echo "  docker compose -f $SCRIPT_DIR/docker-compose.yml down"
