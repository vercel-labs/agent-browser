# Dashboard Endpoint Resolution

This document captures the path and connection rules for the observability dashboard so future work does not reintroduce root-only assumptions.

## Goals

- Keep root-path deployments working exactly as they do today.
- Make the dashboard work when mounted behind a reverse proxy under a path prefix such as `/agent-browser`.
- Keep all browser traffic same-origin with the dashboard page itself.

## Rules

1. The browser must treat the dashboard page URL as the source of truth for its mount path.
2. Dashboard HTTP endpoints must resolve from `dashboard origin + dashboard base path`.
3. Dashboard WebSocket endpoints must resolve from `dashboard origin + dashboard base path`.
4. The `port` query parameter identifies the target session only. It must not be used by browser code to construct a direct `localhost:<port>` network origin.
5. Session-specific HTTP and WebSocket traffic must go through the dashboard proxy routes:
   - `/api/session/<port>/tabs`
   - `/api/session/<port>/status`
   - `/api/session/<port>/stream`
6. Public dashboard assets must resolve under the same dashboard base path as the page itself.
7. The backend should accept both root-mounted requests and requests that retain a proxy path prefix when forwarded upstream.

## Practical Meaning

- If the page is loaded from `/`, requests stay under `/api/...`.
- If the page is loaded from `/agent-browser/`, requests move to `/agent-browser/api/...`.
- If the page is loaded from `/agent-browser/index.html`, requests still move to `/agent-browser/api/...`.

## Non-Goals

- Reintroducing browser-side direct connections to session ports.
- Requiring a separate public origin for normal reverse-proxy deployments.

## Notes

- The default dashboard build should embed a shared base-path placeholder token instead of a hardcoded public path.
- The dashboard server should replace that token at response time using the request's effective mount prefix, so one build artifact works for both `/` and subpath deployments.
- Static export output should not hardcode `/_next/...` asset URLs, because those bypass the dashboard base path.
- Reverse proxies may either strip the base path before forwarding or preserve it. The dashboard should tolerate both.
