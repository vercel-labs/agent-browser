---
name: derive-client
description: Reverse-engineer a website's internal API by recording browser traffic into a HAR file, then generate a standalone client or CLI that calls the endpoints directly, with no browser needed after the first recording. Use when asked to "derive a client", "build a CLI for <site>", "reverse engineer this site's API", "record network requests", "turn this site into an API", or when the same site will be automated repeatedly and direct HTTP calls would beat driving the browser every time.
allowed-tools: Bash(agent-browser:*), Bash(npx agent-browser:*)
---

# Derive an API client from a recorded session

Driving a browser is the right tool for the first visit and the wrong tool for the hundredth. This skill records a site's network traffic once while you use it, then turns the captured requests into a standalone client (script, CLI, or library) that talks to the site's internal API directly.

The recording alone contains everything needed: agent-browser embeds text response bodies (JSON/HTML/JS) in the HAR by default, so endpoint shapes can be studied offline after the browser is closed.

## Workflow

```
1. Record     Start HAR capture, drive the flows you want in the client
2. Identify   Find the real API endpoints among the noise
3. Extract    Pull request shapes, response schemas, and auth material
4. Generate   Write the client, one function per flow
5. Verify     Call every endpoint for real before declaring done
```

## 1. Record

```bash
agent-browser network har start          # embeds text response bodies by default
# ... drive the site: search, open a detail page, paginate, etc. ...
agent-browser network har stop /tmp/site.har
```

- Exercise **every flow the client should support**, and run each one at least twice with different inputs (two search terms, two detail pages). Diffing the recorded URLs reveals which parts are parameters.
- If the site needs login, log in **before** starting the HAR so credentials don't land in the recording unnecessarily. The session cookies are exported separately in step 3.
- `--content all` embeds binary bodies too (base64); `--content none` disables embedding. Per-body cap is 2 MB.

While the session is still open, `agent-browser network requests` and `network request <id>` give the same data interactively — but only the HAR survives navigation and browser close, so prefer it for anything multi-page.

## 2. Identify endpoints

Query the HAR with `jq`:

```bash
# All JSON API calls: method, URL, status
jq -r '.log.entries[]
  | select(.response.content.mimeType | test("json"))
  | "\(.request.method) \(.response.status) \(.request.url)"' /tmp/site.har
```

Ignore analytics and infrastructure noise: telemetry endpoints (`/collect`, `/track`, `/beacon`, `/log`), third-party domains (google-analytics, segment, sentry, datadog, intercom, hotjar), and static assets. The real API is usually first-party, JSON, and correlates with the actions you performed.

## 3. Extract shapes and auth

```bash
# Full detail for one endpoint: request headers, POST body, response body
jq '.log.entries[] | select(.request.url | test("api/search"))
  | {request: {method: .request.method, headers: .request.headers,
     postData: .request.postData.text},
     response: .response.content.text}' /tmp/site.har
```

- **Response schema**: read `.response.content.text` — this is the real payload, use it to derive types.
- **Auth**: compare request headers across endpoints. Look for `authorization`, `cookie`, `x-csrf-token`, `x-api-key`, and site-specific `x-*` headers. Replay only the ones that matter — test by omission in step 5.
- **Cookies**: export the live session with `agent-browser cookies get --json > cookies.json` for the client to load at runtime. Never hardcode cookie values into generated source.

## 4. Generate the client

- One function per recorded flow (`search(query)`, `getItem(id)`), typed from the observed response bodies.
- Auth material (cookies, bearer tokens) loads from a file or environment variable, with a clear error telling the user to re-run the browser login when it expires.
- Reproduce the headers the API actually requires — some sites 403 without a matching `user-agent`, `referer`, or `x-requested-with`.
- Keep pagination, sort, and filter parameters that appeared in the recorded query strings as function options.

## 5. Verify

Call every generated function against the live API and compare the response shape with the recording. Common failures:

| Symptom | Cause | Fix |
|---------|-------|-----|
| 401/403 | Expired or missing session | Re-login via agent-browser, re-export cookies |
| 403/419 on writes | CSRF token is per-session or per-form | Fetch the token endpoint first, or keep that flow browser-driven |
| Works then breaks | Signed/expiring request params | Fall back to the browser for that step; derive the rest |
| Different shape than HAR | A/B tests or geo-dependent responses | Re-record and treat the union as optional fields |

## Caveats

- Internal APIs are unversioned and change without notice — keep the HAR so the client can be re-derived.
- Respect the site's terms of service and rate limits; add delays for bulk fetching.
- HAR files contain live session credentials (cookies, tokens, POST bodies). Treat them like secrets: keep them out of version control and delete them when done.
