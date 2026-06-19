---
name: agentcore
description: Run agent-browser on AWS Bedrock AgentCore cloud browsers. Use when the user wants to use AgentCore, run browser automation on AWS, use a cloud browser with AWS credentials, or needs a managed browser session backed by AWS infrastructure. Triggers include "use agentcore", "run on AWS", "cloud browser with AWS", "bedrock browser", "agentcore session", or any task requiring AWS-hosted browser automation.
allowed-tools: Bash(agent-browser:*), Bash(npx agent-browser:*)
---

# AWS Bedrock AgentCore

Run agent-browser on cloud browser sessions hosted by AWS Bedrock AgentCore. All standard agent-browser commands work identically; the only difference is where the browser runs.

## Setup

Credentials are resolved automatically:

1. Environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, optionally `AWS_SESSION_TOKEN`)
2. AWS CLI fallback (`aws configure export-credentials`), which supports SSO, IAM roles, and named profiles

No additional setup is needed if the user already has working AWS credentials.

## Core Workflow

```bash
# Open a page on an AgentCore cloud browser
agent-browser -p agentcore open https://example.com

# Everything else is the same as local Chrome
agent-browser snapshot -i
agent-browser click @e1
agent-browser screenshot page.png
agent-browser close
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `AGENTCORE_REGION` | AWS region | `us-east-1` |
| `AGENTCORE_BROWSER_ID` | Browser identifier | `aws.browser.v1` |
| `AGENTCORE_PROFILE_ID` | Persistent browser profile (cookies, localStorage) | (none) |
| `AGENTCORE_SESSION_TIMEOUT` | Session timeout in seconds | `3600` |
| `AWS_PROFILE` | AWS CLI profile for credential resolution | `default` |

## Persistent Profiles

Use `AGENTCORE_PROFILE_ID` to persist browser state across sessions. This is useful for maintaining login sessions:

```bash
# First run: log in
AGENTCORE_PROFILE_ID=my-app agent-browser -p agentcore open https://app.example.com/login
agent-browser snapshot -i
agent-browser fill @e1 "user@example.com"
agent-browser fill @e2 "password"
agent-browser click @e3
agent-browser close

# Future runs: already authenticated
AGENTCORE_PROFILE_ID=my-app agent-browser -p agentcore open https://app.example.com/dashboard
```

## Live View

When a session starts, AgentCore prints a Live View URL to stderr. Open it in a browser to watch the session in real time from the AWS Console:

```
Session: abc123-def456
Live View: https://us-east-1.console.aws.amazon.com/bedrock-agentcore/browser/aws.browser.v1/session/abc123-def456#
```

## Region Selection

```bash
# Default: us-east-1
agent-browser -p agentcore open https://example.com

# Explicit region
AGENTCORE_REGION=eu-west-1 agent-browser -p agentcore open https://example.com
```

## Credential Patterns

```bash
# Explicit credentials (CI/CD, scripts)
export AWS_ACCESS_KEY_ID=AKIA...
export AWS_SECRET_ACCESS_KEY=...
agent-browser -p agentcore open https://example.com

# SSO (interactive)
aws sso login --profile my-profile
AWS_PROFILE=my-profile agent-browser -p agentcore open https://example.com

# IAM role / default credential chain
agent-browser -p agentcore open https://example.com
```

## Using with AGENT_BROWSER_PROVIDER

Set the provider via environment variable to avoid passing `-p agentcore` on every command:

```bash
export AGENT_BROWSER_PROVIDER=agentcore
export AGENTCORE_REGION=us-east-2

agent-browser open https://example.com
agent-browser snapshot -i
agent-browser click @e1
agent-browser close
```

## Common Issues

**"Failed to run aws CLI"** means AWS CLI is not installed or not in PATH. Either install it or set `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` directly.

**"AWS CLI failed: ... Run 'aws sso login'"** means SSO credentials have expired. Run `aws sso login` to refresh them.

**Session timeout:** The default is 3600 seconds (1 hour). For longer tasks, increase with `AGENTCORE_SESSION_TIMEOUT=7200`.
