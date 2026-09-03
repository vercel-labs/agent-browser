# Proxy Support

Proxy configuration for geo-testing, rate limiting avoidance, and corporate environments.

**Related**: [commands.md](commands.md) for global options, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Basic Proxy Configuration](#basic-proxy-configuration)
- [Authenticated Proxy](#authenticated-proxy)
- [SOCKS Proxy](#socks-proxy)
- [Proxy Bypass](#proxy-bypass)
- [Common Use Cases](#common-use-cases)
- [Verifying Proxy Connection](#verifying-proxy-connection)
- [Troubleshooting](#troubleshooting)
- [Best Practices](#best-practices)

## Basic Proxy Configuration

Use the `--proxy` flag or set proxy via environment variable:

```bash
# Via CLI flag
agent-browser --proxy "http://proxy.example.com:8080" open https://example.com

# Via environment variable
export HTTP_PROXY="http://proxy.example.com:8080"
agent-browser open https://example.com

# HTTPS proxy
export HTTPS_PROXY="https://proxy.example.com:8080"
agent-browser open https://example.com

# Both
export HTTP_PROXY="http://proxy.example.com:8080"
export HTTPS_PROXY="http://proxy.example.com:8080"
agent-browser open https://example.com
```

## Authenticated Proxy

For proxies requiring authentication:

```bash
# Include credentials in URL
export HTTP_PROXY="http://username:password@proxy.example.com:8080"
agent-browser open https://example.com
```

## SOCKS Proxy

```bash
# SOCKS5 proxy
export ALL_PROXY="socks5://proxy.example.com:1080"
agent-browser open https://example.com

# SOCKS5 with auth
export ALL_PROXY="socks5://user:pass@proxy.example.com:1080"
agent-browser open https://example.com
```

## Proxy Bypass

Skip proxy for specific domains using `--proxy-bypass` or `NO_PROXY`:

```bash
# Via CLI flag
agent-browser --proxy "http://proxy.example.com:8080" --proxy-bypass "localhost,*.internal.com" open https://example.com

# Via environment variable
export NO_PROXY="localhost,127.0.0.1,.internal.company.com"
agent-browser open https://internal.company.com  # Direct connection
agent-browser open https://external.com          # Via proxy
```

## Common Use Cases

### Geo-Location Testing

```bash
#!/bin/bash
# Test site from different regions using geo-located proxies

PROXIES=(
    "http://us-proxy.example.com:8080"
    "http://eu-proxy.example.com:8080"
    "http://asia-proxy.example.com:8080"
)

for proxy in "${PROXIES[@]}"; do
    export HTTP_PROXY="$proxy"
    export HTTPS_PROXY="$proxy"

    region=$(echo "$proxy" | grep -oP '^\w+-\w+')
    echo "Testing from: $region"

    agent-browser --session "$region" open https://example.com
    agent-browser --session "$region" screenshot "./screenshots/$region.png"
    agent-browser --session "$region" close
done
```

### Rotating Proxies for Scraping

```bash
#!/bin/bash
# Rotate through proxy list to avoid rate limiting

PROXY_LIST=(
    "http://proxy1.example.com:8080"
    "http://proxy2.example.com:8080"
    "http://proxy3.example.com:8080"
)

URLS=(
    "https://site.com/page1"
    "https://site.com/page2"
    "https://site.com/page3"
)

for i in "${!URLS[@]}"; do
    proxy_index=$((i % ${#PROXY_LIST[@]}))
    export HTTP_PROXY="${PROXY_LIST[$proxy_index]}"
    export HTTPS_PROXY="${PROXY_LIST[$proxy_index]}"

    agent-browser open "${URLS[$i]}"
    agent-browser get text body > "output-$i.txt"
    agent-browser close

    sleep 1  # Polite delay
done
```

### Corporate Network Access

```bash
#!/bin/bash
# Access internal sites via corporate proxy

export HTTP_PROXY="http://corpproxy.company.com:8080"
export HTTPS_PROXY="http://corpproxy.company.com:8080"
export NO_PROXY="localhost,127.0.0.1,.company.com"

# External sites go through proxy
agent-browser open https://external-vendor.com

# Internal sites bypass proxy
agent-browser open https://intranet.company.com
```

## Verifying Proxy Connection

```bash
# Check your apparent IP
agent-browser open https://httpbin.org/ip
agent-browser get text body
# Should show proxy's IP, not your real IP
```

## Troubleshooting

### Proxy Connection Failed

```bash
# Test proxy connectivity first
curl -x http://proxy.example.com:8080 https://httpbin.org/ip

# Check if proxy requires auth
export HTTP_PROXY="http://user:pass@proxy.example.com:8080"
```

### SSL/TLS Errors Through Proxy

Some proxies perform SSL inspection with a custom CA certificate. Trust only that CA:

```bash
# Recommended: trust the proxy's CA certificate
agent-browser --ca-cert /etc/ssl/certs/proxy-ca.crt open https://example.com

# Via environment variable
export AGENT_BROWSER_CA_CERT=/etc/ssl/certs/proxy-ca.crt
agent-browser open https://example.com
```

On Linux, `--ca-cert` imports the certificate or PEM bundle into an isolated NSS database used only by that locally launched Chromium process. Certificate hostname, validity period, and unrelated authority verification stay enabled. Later commands retain the CA when they omit the flag. Use `--no-ca-cert` to clear it. Different certificate content or an explicit clear relaunches Chromium without restarting the daemon, while the same content from any path reuses the browser. `agent-browser install --with-deps` installs the required `certutil`; otherwise install `libnss3-tools` on Debian/Ubuntu or `nss-tools` on RPM Linux.

For a local Chromium launch, the option also adds the CA to the CLI's own TLS trust. Browser-side trust cannot be combined with `--profile`, Lightpanda, or `--ignore-https-errors`, and it is not available for local launches on macOS or Windows.

### Two trust stores

An intercepting proxy can break the connection in two different places, and they need different fixes.


| Symptom | Who rejects the certificate | Fix |
| --- | --- | --- |
| `net::ERR_CERT_AUTHORITY_INVALID` on a page | Chromium | `--ca-cert` |
| `CDP WebSocket connect failed: ... UnknownIssuer` | The CLI, before the browser is reached | `--ca-cert` or `--use-system-ca` |

The CLI verifies its own connections (remote CDP over `wss://`, cloud provider APIs) against a root list compiled into the binary, which cannot see a private CA. Two opt-ins widen it:

```bash
# Use the machine's trust store, where the proxy CA is usually already installed
export AGENT_BROWSER_USE_SYSTEM_CA=1
agent-browser --cdp wss://remote.example.com/session open https://example.com

# Or point at the CA bundle directly. SSL_CERT_FILE works too.
agent-browser --ca-cert /etc/pki/ca-trust/source/anchors/proxy-ca.pem --cdp wss://... open https://example.com
```

Neither disables verification. Without one of them the CLI keeps using the built-in roots, so nothing changes for setups that work today. `agent-browser doctor` reports which trust store is active.

With `--cdp`, `--auto-connect`, or a provider, `--ca-cert` configures only the CLI's connections. It cannot change the trust store of a browser that agent-browser did not launch. `--use-system-ca` always affects only CLI TLS.

Which one to reach for depends on where the CA already lives, and that differs by platform:

| Platform | Where a proxy CA usually lives | Use |
| --- | --- | --- |
| Linux, containers, Vercel Sandbox | a PEM bundle, with `SSL_CERT_FILE` already pointing at it | nothing; it is picked up. Otherwise `--ca-cert <path>` |
| macOS | the Keychain, installed by MDM. There is no `SSL_CERT_FILE` by default | `--use-system-ca` |
| Windows | the Windows certificate store | `--use-system-ca` |
| Any, when you have the certificate as a file | wherever you saved it | `--ca-cert <path>` |

`--ca-cert` takes a path, so it cannot reach a CA that lives only in the macOS Keychain or the Windows certificate store. That is what `--use-system-ca` is for. On Linux the two overlap, because the system store is a file there.

### Vercel Sandbox

A Vercel Sandbox network policy that rewrites requests terminates TLS and re-signs with the Vercel proxy CA, which the sandbox already installs. Set `AGENT_BROWSER_USE_SYSTEM_CA=1` in the sandbox so the CLI picks it up.

Without the CA certificate on hand, fall back to ignoring every certificate error:

```bash
# For testing only - not recommended for production
agent-browser open https://example.com --ignore-https-errors
```

### Slow Performance

```bash
# Use proxy only when necessary
export NO_PROXY="*.cdn.com,*.static.com"  # Direct CDN access
```

## Best Practices

1. **Use environment variables** - Don't hardcode proxy credentials
2. **Set NO_PROXY appropriately** - Avoid routing local traffic through proxy
3. **Test proxy before automation** - Verify connectivity with simple requests
4. **Handle proxy failures gracefully** - Implement retry logic for unstable proxies
5. **Rotate proxies for large scraping jobs** - Distribute load and avoid bans
