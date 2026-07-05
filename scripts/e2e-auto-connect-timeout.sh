#!/usr/bin/env bash
set -Eeuo pipefail

# Run a real Chrome auto-connect lifecycle in a private Linux network
# namespace. The browser profile, daemon socket, and session are all created
# below a temporary directory and are removed on exit.
#
# Build the isolated binary first:
#   CARGO_TARGET_DIR=/tmp/agent-browser-worker-target cargo build --manifest-path cli/Cargo.toml --bin agent-browser
# Then run this script from the repository root:
#   ./scripts/e2e-auto-connect-timeout.sh
#
# Override the binary or browser when needed:
#   AGENT_BROWSER_E2E_BINARY=/path/to/agent-browser AGENT_BROWSER_E2E_CHROME=/path/to/chrome ./scripts/e2e-auto-connect-timeout.sh

if ! command -v unshare >/dev/null 2>&1; then
    echo "unshare is required" >&2
    exit 2
fi

if ! command -v ip >/dev/null 2>&1; then
    echo "ip is required to enable loopback in the private network namespace" >&2
    exit 2
fi

for required in python3 curl; do
    if ! command -v "$required" >/dev/null 2>&1; then
        echo "$required is required by the isolated Chrome fixture" >&2
        exit 2
    fi
done

BINARY="${AGENT_BROWSER_E2E_BINARY:-/tmp/agent-browser-worker-target/debug/agent-browser}"
CHROME="${AGENT_BROWSER_E2E_CHROME:-}"

if [[ -z "$CHROME" ]]; then
    for candidate in chromium google-chrome-stable google-chrome chromium-browser; do
        if command -v "$candidate" >/dev/null 2>&1; then
            CHROME="$(command -v "$candidate")"
            break
        fi
    done
fi

if [[ ! -x "$BINARY" ]]; then
    echo "isolated agent-browser binary not found or not executable: $BINARY" >&2
    echo "Build it with: CARGO_TARGET_DIR=/tmp/agent-browser-worker-target cargo build --manifest-path cli/Cargo.toml --bin agent-browser" >&2
    exit 2
fi

if [[ -z "$CHROME" || ! -x "$CHROME" ]]; then
    echo "Chrome/Chromium executable not found" >&2
    exit 2
fi

PATH_VALUE="${PATH:-/usr/bin:/bin}"

# Do not inherit the caller's HOME, proxy, agent-browser configuration, or
# browser-related environment into the namespace. The inner shell sets only
# the temporary values used by this fixture.
exec env -i "PATH=$PATH_VALUE" LC_ALL=C unshare -Urn bash -s -- "$BINARY" "$CHROME" <<'INNER_SCRIPT'
set -Eeuo pipefail

BINARY="$1"
CHROME="$2"

ip link set lo up

RUN_ROOT="$(mktemp -d /tmp/agent-browser-real-e2e.XXXXXX)"
HOME="$RUN_ROOT/home"
PROFILE="$HOME/.config/google-chrome"
SOCKET_DIR="$RUN_ROOT/socket"
SESSION="real-auto-connect-e2e"
mkdir -p "$PROFILE" "$SOCKET_DIR"
cd "$RUN_ROOT"

export HOME
export AGENT_BROWSER_SOCKET_DIR="$SOCKET_DIR"
export NO_COLOR=1
unset AGENT_BROWSER_CDP AGENT_BROWSER_PROVIDER AGENT_BROWSER_PROFILE AGENT_BROWSER_STATE AGENT_BROWSER_ARGS
unset AGENT_BROWSER_EXECUTABLE_PATH AGENT_BROWSER_ENGINE AGENT_BROWSER_HEADED AGENT_BROWSER_HEADLESS
unset AGENT_BROWSER_RESTORE AGENT_BROWSER_SESSION_NAME AGENT_BROWSER_ACTION_POLICY AGENT_BROWSER_CONFIRM_ACTIONS
unset AGENT_BROWSER_DEFAULT_TIMEOUT AGENT_BROWSER_IDLE_TIMEOUT_MS AGENT_BROWSER_PLUGINS AGENT_BROWSER_INIT_SCRIPTS
unset AGENT_BROWSER_ENABLE AGENT_BROWSER_NO_WEBMCP AGENT_BROWSER_WEBGPU AGENT_BROWSER_CA_CERT AGENT_BROWSER_CLEAR_CA_CERT
unset AGENT_BROWSER_PROXY AGENT_BROWSER_PROXY_BYPASS AGENT_BROWSER_PIN_TAB AGENT_BROWSER_AUTO_CONNECT AGENT_BROWSER_CONFIG
unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy NO_PROXY no_proxy

CHROME_PID=""
SERVER_PID=""
RELAY_PID=""
DAEMON_PID=""

stop_owned_process() {
    local pid="$1"
    [[ -n "$pid" ]] || return 0
    if ! kill -0 "$pid" 2>/dev/null; then
        return 0
    fi
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 20); do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.05
    done
    kill -KILL "$pid" 2>/dev/null || true
}

cleanup() {
    set +e
    stop_owned_process "$DAEMON_PID"
    stop_owned_process "$RELAY_PID"
    stop_owned_process "$SERVER_PID"
    stop_owned_process "$CHROME_PID"
    rm -rf "$RUN_ROOT"
}
trap cleanup EXIT INT TERM

python3 - "$RUN_ROOT/fixture.url" >"$RUN_ROOT/server.log" 2>&1 <<'PY' &
import http.server
import pathlib
import sys


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = (
            b"<!doctype html><html><head><title>isolated-auto-connect</title>"
            b"</head><body><main id='status'>ready</main></body></html>"
        )
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
pathlib.Path(sys.argv[1]).write_text(
    f"http://127.0.0.1:{server.server_address[1]}/fixture.html"
)
server.serve_forever()
PY
SERVER_PID=$!

for _ in $(seq 1 100); do
    [[ -s "$RUN_ROOT/fixture.url" ]] && break
    sleep 0.05
done
if [[ ! -s "$RUN_ROOT/fixture.url" ]]; then
    cat "$RUN_ROOT/server.log" >&2
    echo "fixture server did not start" >&2
    exit 1
fi
FIXTURE_URL="$(cat "$RUN_ROOT/fixture.url")"
SECOND_URL="$FIXTURE_URL?step=omitted"

"$CHROME" \
    --headless=new \
    --no-sandbox \
    --disable-gpu \
    --disable-dev-shm-usage \
    --no-first-run \
    --no-default-browser-check \
    --disable-background-networking \
    --disable-component-update \
    --disable-sync \
    --disable-extensions \
    --remote-debugging-address=127.0.0.1 \
    --remote-debugging-port=0 \
    --user-data-dir="$PROFILE" \
    about:blank >"$RUN_ROOT/chrome.log" 2>&1 &
CHROME_PID=$!

ACTIVE_PORT="$PROFILE/DevToolsActivePort"
for _ in $(seq 1 200); do
    [[ -s "$ACTIVE_PORT" ]] && break
    if ! kill -0 "$CHROME_PID" 2>/dev/null; then
        cat "$RUN_ROOT/chrome.log" >&2
        echo "Chrome exited before writing DevToolsActivePort" >&2
        exit 1
    fi
    sleep 0.05
done
if [[ ! -s "$ACTIVE_PORT" ]]; then
    cat "$RUN_ROOT/chrome.log" >&2
    echo "DevToolsActivePort did not appear at $ACTIVE_PORT" >&2
    exit 1
fi

CDP_PORT="$(sed -n '1p' "$ACTIVE_PORT")"
CDP_PATH="$(sed -n '2p' "$ACTIVE_PORT")"
if [[ ! "$CDP_PORT" =~ ^[0-9]+$ || -z "$CDP_PATH" ]]; then
    echo "invalid DevToolsActivePort contents:" >&2
    cat "$ACTIVE_PORT" >&2
    exit 1
fi
echo "Chrome PID $CHROME_PID exposed DevToolsActivePort on port $CDP_PORT at $PROFILE"

baseline_targets="$RUN_ROOT/baseline-targets.json"
curl -fsS "http://127.0.0.1:$CDP_PORT/json/list" >"$baseline_targets"
BASELINE_PAGE_COUNT="$(python3 - "$baseline_targets" <<'PY'
import json
import sys

targets = json.load(open(sys.argv[1]))
print(sum(target.get("type") == "page" for target in targets))
PY
)"
echo "Chrome baseline page targets: $BASELINE_PAGE_COUNT"

# Put a transparent relay in front of Chrome's real CDP port. Auto-connect
# reads the relay port from DevToolsActivePort, while the relay forwards the
# browser WebSocket unchanged to the original dynamic Chrome endpoint.
RELAY_CONTROL_DIR="$RUN_ROOT/relay-control"
RELAY_PORT_FILE="$RUN_ROOT/relay.port"
RELAY_COUNT_FILE="$RUN_ROOT/relay.count"
mkdir -p "$RELAY_CONTROL_DIR"
python3 - "$CDP_PORT" "$RELAY_CONTROL_DIR" "$RELAY_PORT_FILE" "$RELAY_COUNT_FILE" >"$RUN_ROOT/relay.log" 2>&1 <<'PY' &
import asyncio
import pathlib
import socket
import struct
import sys


target_port = int(sys.argv[1])
control_dir = pathlib.Path(sys.argv[2])
port_file = pathlib.Path(sys.argv[3])
count_file = pathlib.Path(sys.argv[4])
connections = []
accepted = 0


def write_count():
    count_file.write_text(str(accepted))


def abort_writer(writer):
    sock = writer.get_extra_info("socket")
    if sock is not None:
        try:
            sock.setsockopt(
                socket.SOL_SOCKET,
                socket.SO_LINGER,
                struct.pack("ii", 1, 0),
            )
        except OSError:
            pass
    writer.close()


async def close_writer(writer):
    abort_writer(writer)
    try:
        await writer.wait_closed()
    except (ConnectionError, OSError):
        pass


async def forward(reader, writer):
    try:
        while True:
            data = await reader.read(65536)
            if not data:
                return
            writer.write(data)
            await writer.drain()
    except (ConnectionError, OSError):
        return


async def handle_client(client_reader, client_writer):
    global accepted

    try:
        server_reader, server_writer = await asyncio.open_connection(
            "127.0.0.1", target_port
        )
    except (ConnectionError, OSError):
        await close_writer(client_writer)
        return

    pair = (client_writer, server_writer)
    connections.append(pair)
    accepted += 1
    write_count()
    tasks = [
        asyncio.create_task(forward(client_reader, server_writer)),
        asyncio.create_task(forward(server_reader, client_writer)),
    ]
    try:
        await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
    finally:
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        if pair in connections:
            connections.remove(pair)
        await asyncio.gather(
            close_writer(client_writer),
            close_writer(server_writer),
            return_exceptions=True,
        )


async def drop_all_connections():
    current = list(connections)
    await asyncio.gather(
        *(close_writer(writer) for pair in current for writer in pair),
        return_exceptions=True,
    )


async def watch_control():
    while True:
        for request in sorted(control_dir.glob("*.request")):
            ack = request.with_suffix(".ack")
            try:
                request.unlink()
            except FileNotFoundError:
                continue
            await drop_all_connections()
            ack.write_text(str(accepted))
        await asyncio.sleep(0.01)


async def main():
    write_count()
    server = await asyncio.start_server(handle_client, "127.0.0.1", 0)
    relay_port = server.sockets[0].getsockname()[1]
    port_file.write_text(str(relay_port))
    control_task = asyncio.create_task(watch_control())
    try:
        async with server:
            await server.serve_forever()
    finally:
        control_task.cancel()
        await asyncio.gather(control_task, return_exceptions=True)


asyncio.run(main())
PY
RELAY_PID=$!

for _ in $(seq 1 100); do
    [[ -s "$RELAY_PORT_FILE" ]] && break
    if ! kill -0 "$RELAY_PID" 2>/dev/null; then
        cat "$RUN_ROOT/relay.log" >&2
        echo "CDP relay exited before publishing its port" >&2
        exit 1
    fi
    sleep 0.05
done
if [[ ! -s "$RELAY_PORT_FILE" ]]; then
    cat "$RUN_ROOT/relay.log" >&2
    echo "CDP relay did not publish a port" >&2
    exit 1
fi
RELAY_PORT="$(cat "$RELAY_PORT_FILE")"
if [[ ! "$RELAY_PORT" =~ ^[0-9]+$ ]]; then
    echo "invalid CDP relay port: $RELAY_PORT" >&2
    exit 1
fi

relay_active_port="$ACTIVE_PORT.relay"
printf '%s\n%s\n' "$RELAY_PORT" "$CDP_PATH" >"$relay_active_port"
mv "$relay_active_port" "$ACTIVE_PORT"
echo "CDP relay PID $RELAY_PID listening on port $RELAY_PORT and forwarding to Chrome port $CDP_PORT"

run_cli() {
    local label="$1"
    shift
    local allow_relaunch=false
    if [[ "${1:-}" == "--allow-relaunch" ]]; then
        allow_relaunch=true
        shift
    fi
    local output="$RUN_ROOT/$label.json"
    local stderr="$RUN_ROOT/$label.stderr"

    echo "Running $label"
    if ! "$@" >"$output" 2>"$stderr"; then
        echo "$label failed" >&2
        cat "$stderr" >&2
        cat "$output" >&2
        exit 1
    fi

    python3 - "$label" "$output" "$allow_relaunch" <<'PY'
import json
import sys

label, path, allow_relaunch = sys.argv[1:]
response = json.load(open(path))
if response.get("success") is not True:
    raise SystemExit(f"{label} returned failure: {json.dumps(response)}")
lifecycle = response.get("data", {}).get("lifecycle", {})
relaunched = lifecycle.get("relaunchedBrowser") is True
if relaunched and allow_relaunch != "true":
    raise SystemExit(f"{label} relaunched the browser: {json.dumps(response)}")
if allow_relaunch == "true" and not relaunched:
    raise SystemExit(f"{label} did not report browser recovery: {json.dumps(response)}")
status = "browser recovered" if relaunched else "daemon lifecycle reused"
print(f"{label}: success, {status}")
PY
}

extract_target() {
    local label="$1"
    local path="$RUN_ROOT/$label.json"
    python3 - "$label" "$path" <<'PY'
import json
import sys

label, path = sys.argv[1:]
response = json.load(open(path))
target_id = response.get("data", {}).get("targetId")
if not isinstance(target_id, str) or not target_id:
    raise SystemExit(f"{label} did not return a target id: {json.dumps(response)}")
print(target_id)
PY
}

extract_result() {
    local label="$1"
    local path="$RUN_ROOT/$label.json"
    python3 - "$label" "$path" <<'PY'
import json
import sys

label, path = sys.argv[1:]
response = json.load(open(path))
data = response.get("data", {})
if "result" not in data:
    raise SystemExit(f"{label} did not return an eval result: {json.dumps(response)}")
print(json.dumps(data["result"]))
PY
}

extract_tab_stats() {
    local label="$1"
    local expected_target="$2"
    local path="$RUN_ROOT/$label.json"
    python3 - "$label" "$expected_target" "$BASELINE_PAGE_COUNT" "$path" <<'PY'
import json
import sys

label, expected_target, baseline_count, path = sys.argv[1:]
response = json.load(open(path))
tabs = response.get("data", {}).get("tabs")
if not isinstance(tabs, list):
    raise SystemExit(f"{label} did not return a tab list: {json.dumps(response)}")
pages = [tab for tab in tabs if tab.get("type") == "page"]
active = [tab for tab in pages if tab.get("active") is True]
if len(active) != 1 or active[0].get("targetId") != expected_target:
    raise SystemExit(f"{label} active target changed: {json.dumps(response)}")
expected_count = int(baseline_count) + 1
if len(pages) != expected_count:
    raise SystemExit(
        f"{label} changed page count: expected {expected_count}, got {len(pages)}: {json.dumps(response)}"
    )
blank_count = sum(tab.get("url") == "about:blank" for tab in pages)
if blank_count != int(baseline_count):
    raise SystemExit(
        f"{label} changed blank-tab count: expected {baseline_count}, got {blank_count}: {json.dumps(response)}"
    )
print(f"{label}: {len(pages)} page targets, one pinned active target, no extra blank tabs")
PY
}

relay_count() {
    cat "$RELAY_COUNT_FILE"
}

wait_for_relay_count() {
    local expected="$1"
    for _ in $(seq 1 100); do
        if [[ -s "$RELAY_COUNT_FILE" ]] && [[ "$(relay_count)" == "$expected" ]]; then
            return 0
        fi
        sleep 0.05
    done
    echo "CDP relay connection count did not reach $expected: $(relay_count 2>/dev/null || echo unavailable)" >&2
    exit 1
}

drop_relay_connection() {
    local name="$1"
    local request="$RELAY_CONTROL_DIR/$name.request"
    local ack="$RELAY_CONTROL_DIR/$name.ack"
    rm -f "$request" "$ack"
    : >"$request"
    for _ in $(seq 1 100); do
        [[ -f "$ack" ]] && break
        sleep 0.05
    done
    if [[ ! -f "$ack" ]]; then
        cat "$RUN_ROOT/relay.log" >&2
        echo "CDP relay did not acknowledge $name" >&2
        exit 1
    fi
    echo "CDP relay dropped active connection $name"
    sleep 0.2
}

assert_chrome_target_exists() {
    local label="$1"
    local targets="$RUN_ROOT/$label-chrome-targets.json"
    curl -fsS "http://127.0.0.1:$CDP_PORT/json/list" >"$targets"
    python3 - "$label" "$FIRST_TARGET" "$targets" <<'PY'
import json
import sys

label, expected_target, path = sys.argv[1:]
targets = json.load(open(path))
if not any(
    target.get("type") == "page" and target.get("id") == expected_target
    for target in targets
):
    raise SystemExit(
        f"{label}: Chrome no longer advertises target {expected_target}: {json.dumps(targets)}"
    )
print(f"{label}: Chrome still advertises target {expected_target}")
PY
}

run_cli first env AGENT_BROWSER_AUTO_CONNECT_TIMEOUT=60000 "$BINARY" --json --session "$SESSION" --auto-connect --pin-tab open "$FIXTURE_URL"
FIRST_TARGET="$(extract_target first)"
python3 - "$RUN_ROOT/first.json" "$FIXTURE_URL" <<'PY'
import json
import sys

response = json.load(open(sys.argv[1]))
expected = sys.argv[2]
actual = response.get("data", {}).get("url")
if actual != expected:
    raise SystemExit(f"first navigation URL mismatch: expected {expected}, got {actual}")
PY

DAEMON_PID="$(cat "$SOCKET_DIR/$SESSION.pid")"
if [[ ! "$DAEMON_PID" =~ ^[0-9]+$ ]] || ! kill -0 "$DAEMON_PID" 2>/dev/null; then
    echo "daemon PID is not alive: $DAEMON_PID" >&2
    exit 1
fi
echo "Daemon PID $DAEMON_PID started once"
wait_for_relay_count 1
echo "CDP relay accepted exactly one browser connection"
daemon_env="$RUN_ROOT/daemon.env"
tr '\0' '\n' <"/proc/$DAEMON_PID/environ" >"$daemon_env"
if ! grep -Fxq 'AGENT_BROWSER_AUTO_CONNECT=1' "$daemon_env"; then
    echo "daemon does not retain AGENT_BROWSER_AUTO_CONNECT=1 for implicit recovery" >&2
    exit 1
fi

run_cli first_tabs env AGENT_BROWSER_AUTO_CONNECT_TIMEOUT=60000 "$BINARY" --json --session "$SESSION" --auto-connect tab list
extract_tab_stats first_tabs "$FIRST_TARGET"
if [[ "$(cat "$SOCKET_DIR/$SESSION.pid")" != "$DAEMON_PID" ]]; then
    echo "first tab-list command restarted the daemon" >&2
    exit 1
fi

# The second command omits AGENT_BROWSER_AUTO_CONNECT_TIMEOUT entirely.
run_cli second env -u AGENT_BROWSER_AUTO_CONNECT_TIMEOUT "$BINARY" --json --session "$SESSION" --auto-connect navigate "$SECOND_URL"
SECOND_TARGET="$(extract_target second)"
if [[ "$SECOND_TARGET" != "$FIRST_TARGET" ]]; then
    echo "navigate switched CDP target: $FIRST_TARGET -> $SECOND_TARGET" >&2
    exit 1
fi
if [[ "$(cat "$SOCKET_DIR/$SESSION.pid")" != "$DAEMON_PID" ]]; then
    echo "navigate restarted the daemon" >&2
    exit 1
fi

run_cli second_eval env -u AGENT_BROWSER_AUTO_CONNECT_TIMEOUT "$BINARY" --json --session "$SESSION" --auto-connect eval "document.querySelector('#status').textContent"
if [[ "$(extract_result second_eval)" != '"ready"' ]]; then
    echo "DOM evaluation did not return ready" >&2
    exit 1
fi
if [[ "$(cat "$SOCKET_DIR/$SESSION.pid")" != "$DAEMON_PID" ]]; then
    echo "omitted-timeout eval restarted the daemon" >&2
    exit 1
fi

run_cli third env AGENT_BROWSER_AUTO_CONNECT_TIMEOUT=30000 "$BINARY" --json --session "$SESSION" --auto-connect eval "document.title"
if [[ "$(extract_result third)" != '"isolated-auto-connect"' ]]; then
    echo "DOM title evaluation did not return isolated-auto-connect" >&2
    exit 1
fi
if [[ "$(cat "$SOCKET_DIR/$SESSION.pid")" != "$DAEMON_PID" ]]; then
    echo "changed-timeout eval restarted the daemon" >&2
    exit 1
fi

run_cli third_tabs env AGENT_BROWSER_AUTO_CONNECT_TIMEOUT=30000 "$BINARY" --json --session "$SESSION" --auto-connect tab list
extract_tab_stats third_tabs "$FIRST_TARGET"
if [[ "$(cat "$SOCKET_DIR/$SESSION.pid")" != "$DAEMON_PID" ]]; then
    echo "final tab-list command restarted the daemon" >&2
    exit 1
fi

if [[ "$(relay_count)" != 1 ]]; then
    echo "unexpected CDP relay connection count before recovery: $(relay_count)" >&2
    exit 1
fi
drop_relay_connection recovery_changed
assert_chrome_target_exists after_changed_drop
run_cli recovery_changed --allow-relaunch env AGENT_BROWSER_AUTO_CONNECT_TIMEOUT=30000 "$BINARY" --json --session "$SESSION" eval "document.querySelector('#status').textContent"
if [[ "$(extract_result recovery_changed)" != '"ready"' ]]; then
    echo "changed-timeout recovery DOM evaluation did not return ready" >&2
    exit 1
fi
if [[ "$(cat "$SOCKET_DIR/$SESSION.pid")" != "$DAEMON_PID" ]]; then
    echo "changed-timeout recovery restarted the daemon" >&2
    exit 1
fi
wait_for_relay_count 2
echo "Changed-timeout implicit recovery opened exactly one new CDP connection"

run_cli recovery_changed_tabs "$BINARY" --json --session "$SESSION" tab list
extract_tab_stats recovery_changed_tabs "$FIRST_TARGET"
if [[ "$(relay_count)" != 2 ]]; then
    echo "changed-timeout recovery opened an unexpected number of CDP connections: $(relay_count)" >&2
    exit 1
fi

drop_relay_connection recovery_omitted
assert_chrome_target_exists after_omitted_drop
run_cli recovery_omitted --allow-relaunch env -u AGENT_BROWSER_AUTO_CONNECT_TIMEOUT "$BINARY" --json --session "$SESSION" eval "document.title"
if [[ "$(extract_result recovery_omitted)" != '"isolated-auto-connect"' ]]; then
    echo "omitted-timeout recovery DOM evaluation did not return isolated-auto-connect" >&2
    exit 1
fi
if [[ "$(cat "$SOCKET_DIR/$SESSION.pid")" != "$DAEMON_PID" ]]; then
    echo "omitted-timeout recovery restarted the daemon" >&2
    exit 1
fi
wait_for_relay_count 3
echo "Omitted-timeout implicit recovery opened exactly one new CDP connection"

run_cli recovery_omitted_tabs env -u AGENT_BROWSER_AUTO_CONNECT_TIMEOUT "$BINARY" --json --session "$SESSION" tab list
extract_tab_stats recovery_omitted_tabs "$FIRST_TARGET"
if [[ "$(relay_count)" != 3 ]]; then
    echo "omitted-timeout recovery opened an unexpected number of CDP connections: $(relay_count)" >&2
    exit 1
fi

run_cli close "$BINARY" --json --session "$SESSION" close
echo "PASS: real isolated Chrome auto-connect reused daemon PID $DAEMON_PID and CDP target $FIRST_TARGET across stable commands and two implicit recoveries; CDP relay connection count 3"
INNER_SCRIPT
