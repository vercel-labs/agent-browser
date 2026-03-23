# agent-browser Pricing

## Community (Free)

The community edition is **free forever** and includes all core browser automation commands:

| Feature | Community |
|---------|-----------|
| `open`, `close`, `snapshot` | ✓ |
| `click`, `fill`, `get`, `find` | ✓ |
| `screenshot`, `wait`, `scroll` | ✓ |
| `keyboard`, `mouse`, `evaluate` | ✓ |
| `network`, `cookies`, `upgrade`, `install` | ✓ |
| Sessions (single) | ✓ |
| Rate limit | 100 req/h |
| Support | GitHub Issues |

## Pro — $29/month

Unlock advanced automation features for professional users and teams.

| Feature | Pro |
|---------|-----|
| All community features | ✓ |
| Video recording (`--record`) | ✓ |
| Named parallel sessions (`--session`) | ✓ |
| Cloud provider integration (`--provider`) | ✓ |
| Extended rate limit | 2,000 req/h |
| Priority email support | ✓ |
| License seats | 1 |

**Subscribe:** https://buy.stripe.com/agentbrowser-pro-monthly

## Team — $79/month

Everything in Pro, plus collaboration features for engineering teams.

| Feature | Team |
|---------|------|
| All Pro features | ✓ |
| License seats | 5 |
| Team seat management dashboard | ✓ |
| Audit log | ✓ |
| Slack support | ✓ |

**Subscribe:** https://buy.stripe.com/agentbrowser-team-monthly

## Enterprise — Custom

For organizations running agent-browser at scale.

| Feature | Enterprise |
|---------|------------|
| All Team features | ✓ |
| Unlimited seats | ✓ |
| SSO / SAML | ✓ |
| Dedicated support SLA | ✓ |
| Custom integrations | ✓ |
| On-premise deployment | ✓ |

**Contact:** enterprise@agentbrowser.dev

---

## Using a License Key

After purchasing, set the `AGENT_BROWSER_LICENSE_KEY` environment variable:

```bash
# .env or shell profile
export AGENT_BROWSER_LICENSE_KEY="ABP-XXXXXXXX-XXXXXXXX-XXXXXXXX"
```

Or pass it inline:

```bash
AGENT_BROWSER_LICENSE_KEY="ABP-..." agent-browser record session.webm open example.com
```

### Validate your key

```bash
agent-browser license         # human-readable output
agent-browser license --json  # JSON output for scripting
```

Or run the validator directly:

```bash
AGENT_BROWSER_LICENSE_KEY="ABP-..." node scripts/validate-license.js
```

### CI/CD

Store the key as a secret in your CI provider and expose it as `AGENT_BROWSER_LICENSE_KEY`. The validator caches successful checks for 24 hours in `~/.agent-browser/license-cache.json` to minimize network calls.

---

## FAQ

**Can I use the community edition commercially?**
Yes. The community edition is licensed under Apache 2.0 and may be used commercially without restriction.

**What happens when my subscription lapses?**
Pro/Team features are gated by the license check. If validation fails, the CLI falls back to community mode automatically — your automation scripts continue to work for features available in the community tier.

**Is the license check enforced offline?**
A fast offline checksum validation runs on every invocation. A full online check is performed at most once every 24 hours, so connectivity is not required for normal use.

**Can I switch between monthly and annual billing?**
Yes. Annual billing (billed once per year) saves ~17% vs. monthly. Switch at any time from the billing portal.

**Where is my billing portal?**
https://agentbrowser.dev/billing — log in with the email used at purchase.
