# Live Streaming

Stream a session's viewport over WebSocket and drive it with remote input. This is what a remote preview or embedded dashboard connects to: the browser runs wherever the daemon runs (a sandbox, a container, a CI box), and the client renders frames and sends clicks back.

**Related**: [commands.md](commands.md) for full command reference, [SKILL.md](../SKILL.md) for quick start.

## Event subscriptions

A subscription watches for something in the page and stores what happens on the session's event bus, so you can act, end your turn, and learn about the result without polling. The bus runs inside the daemon and adds no port or listener. Every operation goes through the same action policy and output limits as the rest of the CLI.

Chrome is the only supported engine. Other engines return an unsupported error.

```bash
agent-browser events start --selector '#deploy-complete'
agent-browser click '#deploy'
agent-browser events read ev_1a2b3c4d --wait 30000 --ack
```

| Command | What it does |
|---|---|
| `events start` | Arms a subscription and prints its id, head, `alreadySatisfied`, expiry and tab. |
| `events read <id>` | Returns stored envelopes after a cursor, optionally waiting and optionally acknowledging. |
| `events wait <id>` | CLI shorthand for a read that waits and acknowledges. Bound it with `--timeout <ms>`. |
| `events ack <id> <cursor>` | Moves the `consumed` watermark without reading. |
| `events list` | Lists subscriptions with status, reason, rule, owner, head, consumed, dropped count, tab and expiry. |
| `events stop <id>` | Ends a subscription with reason `cancelled`. |
| `events watch <id>` | CLI-only NDJSON loop over read, one envelope per line. |

MCP tools and eve tools expose start, read, ack, list and stop with JSON identical to the CLI. `wait` and `watch` are CLI conveniences.

### Start

```bash
agent-browser events start --selector <css> [--state visible|hidden] [--once] [--ttl <secs>] [--owner <label>] [--key <key>]
```

- `--selector` is a CSS selector in the current document.
- `--state` is `visible` (default) or `hidden`. Selector sources emit `element.visible` and `element.hidden`; this picks which transition is stored.
- `--once` ends the subscription after its first stored event with reason `matched`.
- `--ttl` is in seconds, defaults to 600, and is capped at 1800.
- `--owner` labels the subscription. `ack` and `stop` with an owner only touch subscriptions that carry it.
- `--key` is an idempotency key. Starting again with the same key and rule returns the same id.

`alreadySatisfied` is true when the requested state already holds at arm time. That initial state is reported in `subscription.ready` and creates no event; events are stored on later transitions. The start result also has `head`, the head sequence number, and `cursor`, the cursor at that head, which a first read can pass as `--since`.

At most 20 subscriptions are active. Finished subscriptions stay readable for 10 minutes, and `events list` shows at most 50 entries.

A subscription is bound to the current tab. Closing or crashing the tab ends it with `tab_closed` or `tab_crashed`. Page observers re-arm after navigation. Same-document and new-document navigation each produce `subscription.rearmed`. If the condition already holds in the new document, the initial observation is stored as an event flagged `afterNavigation`, so a click that navigates and then shows the expected element still matches.

### Read, cursors and acks

```bash
agent-browser events read <id> [--since <cursor>] [--wait <ms>] [--limit <n>] [--ack]
```

The result has `status`, `events`, `cursor`, `head`, `consumed` and `waiting`. Keep the returned `cursor` and pass it as the next `--since`. The client holds its own cursor; the daemon stores only the `consumed` watermark.

A cursor is opaque and shaped like `<subscription id>:<seq>`, for example `ev_1a2b3c4d:4`. An omitted `--since` starts at the oldest retained item. A cursor from another subscription or from ahead of the head returns `cursor_invalid`. An id the daemon no longer knows, including after a daemon restart or idle shutdown, returns an explicit daemon-restarted error.

`--limit` caps returned items. `--wait` bounds the blocking time. `waiting` is true when the wait timed out with nothing new. MCP and chat tools clamp their waits instead of raising their own time limits.

A plain read never moves `consumed`. `--ack` advances it to the last sequence number returned, and only when that is above the current value. Replays and limit-truncated reads never move it backwards or past an event that was not returned. `events wait` is a read with both options.

### List and unread events

```bash
agent-browser events list [--owner <label>] [--unread]
```

An entry has `id`, `status`, `reason`, `rule`, `owner`, `head`, `consumed`, `dropped`, `tab` and `expiresAt`. `status` is `waiting` while armed, then `matched`, `timeout`, `cancelled` or `ended`; `reason` carries the specific end reason. `dropped` counts events removed by the buffer bounds.

`--unread` returns only subscriptions whose `head` is above `consumed`. An empty list means nothing is waiting, so a hook or script can branch on it without parsing event data.

End reasons are `matched`, `timeout`, `cancelled`, `tab_closed`, `tab_crashed`, `browser_closed` and `policy_denied`.

### Envelope and markers

Every stored item is an `agent-browser.event.v1` envelope. `meta` is bus-generated and safe for model-visible text. `data` is page-derived, capped and untrusted.

```json
{
  "version": "agent-browser.event.v1",
  "meta": {
    "eventId": "9d0f6a8e-1f5d-4b4e-9a8a-3f7a1c2d4e5f",
    "name": "element.visible",
    "timestamp": "2026-10-21T18:12:44.120Z",
    "cursor": "ev_1a2b3c4d:4",
    "subscriptionId": "ev_1a2b3c4d",
    "session": "default",
    "tab": { "id": "t1", "label": "Deploy", "targetId": "8A2C..." }
  },
  "data": { "state": "visible", "label": "Deploy complete", "afterNavigation": false }
}
```

| Marker | Meaning |
|---|---|
| `subscription.ready` | Initial observation for the subscription or for a new document. A late joiner sharing an observer gets the current state here. |
| `subscription.rearmed` | The page observer re-armed after same-document or new-document navigation. The subscription continues. |
| `subscription.gap` | Events were lost to buffer bounds or CDP lag. `data.missed` carries the count. The subscription continues, and a page observer next stores a current-state observation flagged `afterGap`. |
| `subscription.ended` | The subscription ended. `data.reason` names why. |

A gap means a transition that came and went entirely inside the loss cannot be reported. The `afterGap` observation tells you whether the condition holds now. Neither a gap nor a slow reader ends a subscription.

### Selector recipes

An element appears:

```bash
agent-browser events start --selector '#deploy-complete' --once
agent-browser click '#deploy'
agent-browser events wait ev_1a2b3c4d
```

An element disappears:

```bash
agent-browser events start --selector '#progress' --state hidden --once
agent-browser click '#cancel'
agent-browser events wait ev_1a2b3c4d
```

Wait after a click that navigates:

```bash
agent-browser events start --selector '[role="dialog"]' --once
agent-browser click '#open-settings'
agent-browser events wait ev_1a2b3c4d
```

### CDP tap recipes

Use `--method` to tap an allowlisted CDP event instead of a selector. `--match <json-pointer>=<value>` and `--match-regex <json-pointer>=<regex>` test the projected `data` fields, and every matcher must hold. These recipes were verified on Chrome only.

A dialog opens:

```bash
agent-browser events start --method Page.javascriptDialogOpening --match /type=alert --once
agent-browser eval "setTimeout(() => alert('hello'), 0)"
agent-browser events wait ev_1a2b3c4d
agent-browser dialog dismiss
```

Stores `Page.javascriptDialogOpening` with `data.type`, `data.message` and `data.url`, plus `data.defaultPrompt` on prompts; `/type=alert` narrows it to alerts.

A same-document navigation commits:

```bash
agent-browser events start --method Page.navigatedWithinDocument --once
agent-browser eval "history.pushState({}, '', '/step-2')"
agent-browser events wait ev_1a2b3c4d
```

Stores `Page.navigatedWithinDocument` with `data.frameId` and `data.url`; a full document load stores `Page.frameNavigated` with `data.frameId` and `data.url`, plus `data.parentId` for child frames.

A console message at a chosen level:

```bash
agent-browser events start --method Runtime.consoleAPICalled --match /level=warning --once
agent-browser eval "console.warn('disk space low')"
agent-browser events wait ev_1a2b3c4d
```

Stores `Runtime.consoleAPICalled` with `data.level` and `data.text`; the level is the CDP console type, so `console.warn` matches `warning`.

A page error:

```bash
agent-browser events start --method Runtime.exceptionThrown --once
agent-browser eval "setTimeout(() => { throw new Error('boom') }, 0)"
agent-browser events wait ev_1a2b3c4d
```

Stores `Runtime.exceptionThrown` with `data.text`, `data.url`, `data.lineNumber` and `data.columnNumber`.

A tab opens:

```bash
agent-browser events start --method Target.targetCreated --match /type=page --once
agent-browser tab new https://example.com
agent-browser events wait ev_1a2b3c4d
```

Stores `Target.targetCreated` with `data.targetId`, `data.type` and `data.url`; `/type=page` skips worker and other non-page targets.

A download starts:

```bash
agent-browser events start --method Browser.downloadWillBegin --once
agent-browser click '#download'
agent-browser events wait ev_1a2b3c4d
```

Stores `Browser.downloadWillBegin` with `data.guid`, `data.url`, `data.suggestedFilename` and `data.frameId`; start the session with `--download-path` if you also want the file on disk.

## Harness setup

The bus does not know about any particular harness. The recipes below use only the public verbs, return only bus metadata, and tell the agent to run `events read` for the event itself. Never put page strings from a wake message into a prompt.

### Claude Code

A `Stop` hook that blocks the end of a turn while unread events exist. Write `.claude/hooks/events-unread.sh`:

```bash
#!/bin/bash
set -euo pipefail
pending=$(agent-browser events list --unread --json)
if [ "$(jq '.data.subscriptions | length' <<<"$pending")" -eq 0 ]; then
  exit 0
fi
ids=$(jq -r '[.data.subscriptions[].id] | join(", ")' <<<"$pending")
jq -n --arg ids "$ids" '{
  decision: "block",
  reason: "Browser events are waiting for subscription(s): " + $ids + ". Run agent-browser events read <id> --ack to see them."
}'
```

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PROJECT_DIR}/.claude/hooks/events-unread.sh"
          }
        ]
      }
    ]
  }
}
```

The agent reads and acknowledges the event on the next turn, so the hook stops blocking. Claude Code limits consecutive Stop continuations, and the `stop_hook_active` input is available if the script needs to check it.

An `async` hook with `asyncRewake` waits in the background and wakes the agent on exit code 2. Write `.claude/hooks/await-event.sh`:

```bash
#!/bin/bash
id=$(agent-browser events list --unread --json | jq -r '.data.subscriptions[0].id // empty')
[ -n "$id" ] || exit 0
result=$(agent-browser events wait "$id" --timeout 60000 --json)
[ "$(jq '.data.events | length' <<<"$result")" -gt 0 ] || exit 0
echo "A browser event is ready. Run agent-browser events read $id to see it." >&2
exit 2
```

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PROJECT_DIR}/.claude/hooks/await-event.sh",
            "async": true,
            "asyncRewake": true
          }
        ]
      }
    ]
  }
}
```

A plain background wait is enough when the harness can run a shell task and wake the agent from its output:

```bash
agent-browser events wait ev_1a2b3c4d --timeout 120000
```

### Cursor

A `stop` hook that returns a `followup_message` while unread events exist. Write `.cursor/hooks/events-unread.sh`:

```bash
#!/bin/bash
cat > /dev/null
pending=$(agent-browser events list --unread --json)
if [ "$(jq '.data.subscriptions | length' <<<"$pending")" -eq 0 ]; then
  echo '{}'
  exit 0
fi
ids=$(jq -r '[.data.subscriptions[].id] | join(", ")' <<<"$pending")
jq -n --arg ids "$ids" '{
  followup_message: ("Browser events are waiting for subscription(s): " + $ids + ". Run agent-browser events read <id> --ack to see them.")
}'
```

```json
{
  "version": 1,
  "hooks": {
    "stop": [{ "command": ".cursor/hooks/events-unread.sh" }]
  }
}
```

Cursor applies its own loop limit to repeated follow-ups.

### Shell or script loop

`events watch` writes one envelope per line and is safe to pipe into `jq` or another program:

```bash
id=$(agent-browser events start --selector '#deploy-complete' --json | jq -r '.data.id')
agent-browser click '#deploy'
agent-browser events watch "$id" | jq -c '.meta | {name, cursor, subscriptionId}'
```

The loop prints only bus metadata. Run `agent-browser events read "$id" --ack` to collect the envelopes, or use `events ack` after processing them.

## Contents

- [Event subscriptions](#event-subscriptions)
- [Harness setup](#harness-setup)
- [Enabling the stream](#enabling-the-stream)
- [Connecting](#connecting)
- [Messages from the server](#messages-from-the-server)
- [Messages from the client](#messages-from-the-client)
- [Frame rate and staleness](#frame-rate-and-staleness)
- [Limitations](#limitations)

## Enabling the stream

Streaming is always available; the server binds an OS-assigned localhost port unless told otherwise.

```bash
agent-browser stream status --json     # Report enabled state, port, client count
agent-browser stream enable            # Create the server (--port to pin one)
agent-browser stream disable           # Tear it down
```

`AGENT_BROWSER_STREAM_PORT` pins the port for the whole daemon instead of passing `--port`.

Frame encoding is daemon-wide, read once at startup:

| Variable | Default | Notes |
|---|---|---|
| `AGENT_BROWSER_STREAM_QUALITY` | `80` | 0 to 100, clamped |
| `AGENT_BROWSER_STREAM_MAX_WIDTH` | the viewport | caps the frame, does not resize the page |
| `AGENT_BROWSER_STREAM_MAX_HEIGHT` | the viewport | same |

The live stream requests jpeg, since a `frame` message carries no format field. An explicit `screencast_start` reconfigures the same underlying screencast, so a client can still see the format change mid-stream; sniff the bytes rather than assuming. Measured on a busy page at 1280x720: quality 80 gives ~54 KB per frame, quality 20 gives ~25 KB, and quality 20 at 640x360 gives ~9 KB. An unusable value leaves the default.

Read the port from `stream status --json` rather than assuming one; the OS-assigned default changes per daemon.

## Connecting

Connect a WebSocket client to `ws://127.0.0.1:<port>`. Frame delivery starts automatically once a client attaches, so there is no subscribe message. Browser clients must load from `localhost`, `127.0.0.1`, `::1` or `file://`. Any other origin gets a 403 on the upgrade and needs a proxy.

## Messages from the server

Every message is JSON text with a `type` field.

- `frame`: a viewport image plus its metadata. Delivered latest-first (see below).

```json
{
  "type": "frame",
  "seq": 41,
  "data": "<base64-encoded-jpeg>",
  "metadata": {
    "deviceWidth": 1280, "deviceHeight": 720, "pageScaleFactor": 1,
    "offsetTop": 0, "scrollOffsetX": 0, "scrollOffsetY": 0,
    "timestamp": 1785038682238
  }
}
```

`seq` is a monotonic frame id, echoed back under ack pacing and stable across browser relaunches. `metadata.timestamp` is the capture time in epoch milliseconds, so `Date.now() - timestamp` is the age of the frame being drawn. The other message types:

- `status`: connection state, screencasting flag, viewport size, engine, recording flag. Sent once on connect and again on change.
- `tabs`: the current tab list, sent on connect when tabs are known and on change.
- `url`: on Chrome, full-document, History API, and fragment navigation in the active tab's main frame. Child-frame and background-tab navigation is ignored.
- `console`: console events.

Status, tabs, url, and console travel on an ordered channel: they are delivered in order and are never replaced by a newer message the way frames are. They are not unconditionally durable. A client that falls far enough behind can lag out of that channel and lose messages it never saw, so treat console output as a live feed, not an audit log.

## Messages from the client

```json
{"type": "input_mouse", "eventType": "mousePressed", "x": 40, "y": 40, "button": "left", "clickCount": 1}
{"type": "input_keyboard", "eventType": "keyDown", "key": "a", "text": "a"}
{"type": "input_touch", "eventType": "touchStart", "touchPoints": []}
{"type": "config", "maxFps": 10}
{"type": "config", "pacing": "ack"}
{"type": "ack", "seq": 41}
```

Input dispatches to the browser on a task of its own, separate from frame delivery, so a click is not queued behind a frame write. Events are sent to the browser without waiting for its reply, so a click stays responsive behind a burst of mouse moves. Ordering is preserved: press never overtakes move. Mouse, keyboard, and touch input also reset the daemon idle timer, so an actively driven preview is not shut down by the idle timeout.

`config` sets a per-client frame cap: 1 to 120, or `0` for uncapped (the default). It takes effect immediately, including when it loosens the cap. Each client's cap is its own; other connected clients are unaffected. A value above 120 is clamped to 120; a negative or non-numeric value is ignored, leaving the current cap in place. Neither rejects the connection.

Both settings can also be declared on the URL, which is the only way to have them cover the connection's opening frame: `ws://127.0.0.1:<port>/?pacing=ack&maxFps=10`. A `config` message sent after connecting still wins.

## Frame rate and staleness

The server holds only the newest frame per client and reads it at send time. A frame produced while an earlier one is still being written is skipped, not queued, so the application never builds a backlog.

Push pacing (the default) stops there, and the transport underneath is still ordered: frames already accepted by the socket are delivered in order, so a client that stalls drains whatever the kernel buffered before the writer blocked.

Ack pacing closes that gap. Send `{"type":"config","pacing":"ack"}` and the server keeps at most one frame in flight, waiting for `{"type":"ack","seq":N}` before sending the next. Every frame carries a monotonic `seq`; echo the one you finished rendering. Frames produced while an ack is outstanding replace each other and never reach the socket, so a client that stalls for ten seconds and resumes gets the current page, not ten seconds of history.

Under ack pacing one frame is in flight at a time, so the rate is one frame per transfer plus one acknowledgement round trip. Both the link's bandwidth and its latency bound it, and a link whose bandwidth-delay product exceeds a single frame goes underused. Ack pacing bounds one hop. With a proxy in the path, forward the renderer's acks; acks generated on receipt leave frames queued on the far side. Acks are cumulative, so acknowledging a newer id covers any older one. A client that opts in and then stops acking simply stops receiving frames; status, tabs, url, and console keep flowing.

The two settings compose: `pacing` bounds how much is in flight, `maxFps` bounds the rate. A constrained preview usually wants both.

## Limitations

- Localhost only. Exposing the stream beyond the machine is the embedder's job (tunnel, proxy, or port forward), and the origin allowlist applies to browser clients.
- Frames are images, not a video codec. Bandwidth scales with viewport size and page activity; cap the rate for constrained links.
- In push pacing the server cannot tell a slow renderer from a fast one beyond transport backpressure. Use ack pacing when that distinction matters.
