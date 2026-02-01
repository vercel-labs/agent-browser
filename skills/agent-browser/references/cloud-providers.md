# Cloud Browser Providers

Connect to cloud browser infrastructure for scalable automation without managing local browsers.

## Supported Providers

| Provider | Description |
|----------|-------------|
| `kernel` | [Kernel](https://www.kernel.sh) - Cloud browsers with stealth mode and profiles |
| `browserbase` | [Browserbase](https://browserbase.com) - Headless browser infrastructure |
| `browseruse` | [Browser Use](https://browser-use.com) - AI-native browser automation |

## Basic Usage

```bash
# Via command line flag
agent-browser -p browserbase open https://example.com
agent-browser --provider browserbase open https://example.com

# Via environment variable
export AGENT_BROWSER_PROVIDER="browserbase"
agent-browser open https://example.com
```

## Kernel Setup

[Kernel](https://www.kernel.sh) provides cloud browser infrastructure for AI agents with stealth mode and persistent profiles.

### 1. Get API Key

Sign up at [dashboard.onkernel.com](https://dashboard.onkernel.com) and get your API key.

### 2. Configure Environment

```bash
export KERNEL_API_KEY="your-api-key"
```

### 3. Use with agent-browser

```bash
agent-browser -p kernel open https://example.com
agent-browser -p kernel snapshot -i
agent-browser -p kernel click @e1
agent-browser -p kernel close
```

### Kernel-Specific Options

| Variable | Description | Default |
|----------|-------------|---------|
| `KERNEL_API_KEY` | Required API key | (none) |
| `KERNEL_HEADLESS` | Headless mode (`true`/`false`) | `false` |
| `KERNEL_STEALTH` | Stealth mode to avoid bot detection | `true` |
| `KERNEL_TIMEOUT_SECONDS` | Session timeout in seconds | `300` |
| `KERNEL_PROFILE_NAME` | Profile name for persistent cookies/logins | (none) |

### Profile Persistence with Kernel

Kernel uniquely supports persistent profiles in the cloud:

```bash
# First session - login and save to profile
export KERNEL_PROFILE_NAME="my-app-profile"
agent-browser -p kernel open https://app.example.com/login
agent-browser -p kernel fill @e1 "username"
agent-browser -p kernel fill @e2 "password"
agent-browser -p kernel click @e3
agent-browser -p kernel close  # Cookies saved to profile

# Later sessions - profile auto-loads
export KERNEL_PROFILE_NAME="my-app-profile"
agent-browser -p kernel open https://app.example.com/dashboard  # Already logged in!
```

---

## Browserbase Setup

### 1. Get API Key

Sign up at [browserbase.com](https://browserbase.com) and get your API key.

### 2. Configure Environment

```bash
export BROWSERBASE_API_KEY="your-api-key"
export BROWSERBASE_PROJECT_ID="your-project-id"  # Optional
```

### 3. Use with agent-browser

```bash
agent-browser -p browserbase open https://example.com
agent-browser -p browserbase snapshot -i
agent-browser -p browserbase click @e1
agent-browser -p browserbase screenshot ./result.png
agent-browser -p browserbase close
```

## Browser Use Setup

### 1. Get API Key

Sign up at [browser-use.com](https://browser-use.com) and get your API key.

### 2. Configure Environment

```bash
export BROWSER_USE_API_KEY="your-api-key"
```

### 3. Use with agent-browser

```bash
agent-browser -p browseruse open https://example.com
agent-browser -p browseruse snapshot -i
agent-browser -p browseruse click @e1
agent-browser -p browseruse close
```

## Remote CDP WebSocket

For custom cloud browser setups, connect via WebSocket URL:

```bash
# Connect to remote browser via WebSocket
agent-browser --cdp "wss://browser.example.com/ws" snapshot -i
```

## Provider vs CDP

| Feature | `-p provider` | `--cdp` |
|---------|---------------|---------|
| Setup | API key only | URL/port required |
| Scaling | Provider handles | Self-managed |
| Extensions | Not supported | Supported (local) |
| Best for | Cloud infrastructure | Debugging, custom setups |

## Limitations

When using cloud providers:

- **No extensions** - Browser extensions require local browser
- **No --headed** - Browsers run headless in the cloud (except Kernel: headful by default)
- **No --profile** - Persistent profiles are local-only (except Kernel: use `KERNEL_PROFILE_NAME`)

```bash
# These will error with most providers
agent-browser -p browserbase --extension ./ext  # Error
agent-browser -p browserbase --headed           # Error

# Kernel supports profiles via its own env var
KERNEL_PROFILE_NAME="myprofile" agent-browser -p kernel open https://example.com  # Works!
```

## Common Patterns

### CI/CD Integration

```yaml
# GitHub Actions example
jobs:
  test:
    runs-on: ubuntu-latest
    env:
      AGENT_BROWSER_PROVIDER: browserbase
      BROWSERBASE_API_KEY: ${{ secrets.BROWSERBASE_API_KEY }}
    steps:
      - run: agent-browser open https://example.com
      - run: agent-browser snapshot -i
      - run: agent-browser screenshot ./result.png
```

### Parallel Execution

```bash
#!/bin/bash
# Run multiple browser sessions in cloud
for i in {1..10}; do
    agent-browser -p browserbase --session "worker-$i" open "https://example.com/page$i" &
done
wait

# Collect results
for i in {1..10}; do
    agent-browser -p browserbase --session "worker-$i" screenshot "./result-$i.png"
    agent-browser -p browserbase --session "worker-$i" close
done
```

### Fallback to Local

```bash
#!/bin/bash
# Try cloud first, fall back to local
if [ -n "$BROWSERBASE_API_KEY" ]; then
    agent-browser -p browserbase open https://example.com
else
    agent-browser open https://example.com
fi
```

## Debugging Cloud Sessions

```bash
# Get session info
agent-browser -p browserbase session

# View console logs
agent-browser -p browserbase console

# View errors
agent-browser -p browserbase errors
```

## Cost Optimization

1. **Close sessions promptly** - Cloud sessions may bill by time
2. **Use snapshots efficiently** - Each command is a round-trip
3. **Batch operations** - Combine related actions
4. **Use local for development** - Only use cloud in CI/production
