#!/usr/bin/env python3
"""Test double for the AI gateway, for rehearsal only. It is not the demo path.

Serves an OpenAI compatible POST /v1/chat/completions (streaming SSE with
tool_calls deltas) and GET /v1/models on 127.0.0.1, so the dashboard chat loop
can be exercised without network access or credentials. Each request replays a
fixed agent run through the single agent_browser tool: open the page, look for
hidden elements, snapshot, fill the form, upload the file, start an events
subscription for the element that appears on completion, click Deploy, then end
the turn with a short message. When the conversation's last user message starts
with "[agent-browser event]" (the dashboard delivering the fired subscription),
it replies with the final summary and no tool calls. Refs and selectors are read
from the earlier tool results in the messages, never hardcoded.
"""

import argparse
import json
import re
import sys
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

EVENT_PREFIX = "[agent-browser event]"
EVENT_RE = re.compile(r'Subscription (\S+) on "(.*)": (.*?)\. Page text: "(.*?)"\.', re.S)

HIDDEN_JS = (
    '[...document.querySelectorAll("[hidden]")].map(e => "hidden #" + e.id'
    ' + " <" + e.tagName.toLowerCase() + "> role=" + e.getAttribute("role")'
    ' + " text=" + e.textContent.replace(/[^A-Za-z0-9 .:-]+/g, " ").replace(/ +/g, " ").trim().slice(0, 80)).join(" | ")'
)


class StepError(Exception):
    """Raised when a tool result does not contain what the next step needs."""


def log(message):
    print("[fake-gateway] " + message, file=sys.stderr, flush=True)


def message_text(message):
    """Returns the plain text of a chat message whose content is a string or parts."""
    content = message.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


def parse_request(messages):
    """Extracts the user's inputs and the tool results that follow the last user message."""
    user_texts = [(i, message_text(m)) for i, m in enumerate(messages) if m.get("role") == "user"]
    event_text = user_texts[-1][1] if user_texts and user_texts[-1][1].startswith(EVENT_PREFIX) else None
    prompts = [(i, t) for i, t in user_texts if not t.startswith(EVENT_PREFIX)]
    last_user, text = prompts[-1] if prompts else (-1, "")
    results = [message_text(m) for m in messages[last_user + 1:] if m.get("role") == "tool"]

    url_match = re.search(r"https?://[^\s\"'<>)]+", text)
    url = url_match.group(0).rstrip(".,;:") if url_match else "http://127.0.0.1:4900"
    without_urls = re.sub(r"https?://[^\s\"'<>)]+", " ", text)
    path_match = re.search(r"(?<![\w.~])(/[^\s\"'<>`,;()]+)", without_urls)
    path = path_match.group(1).rstrip(".,;:") if path_match else ""
    name_match = re.search(
        r"project name[:\s]+[\"']?(.+?)[\"']?(?=\s*(?:,|\.(?:\s|$)|\s+and\s|\s+environment\b|$))",
        text,
        re.I,
    )
    project = name_match.group(1).strip() if name_match else "Acme Dashboard"
    env_match = re.search(r"\b(production|preview|development)\b", text, re.I)
    environment = env_match.group(1).capitalize() if env_match else "Production"
    return {"url": url, "path": path, "project": project, "environment": environment, "results": results, "event": event_text}


def find_ref(snapshot, name_pattern, roles=None):
    """Finds the ref of the first snapshot line whose accessible name matches name_pattern."""
    for line in snapshot.splitlines():
        m = re.match(r'\s*-\s*([\w-]+)\s+"([^"]*)"[^\n]*?\bref=(e\d+)', line)
        if not m:
            continue
        role, name, ref = m.groups()
        if roles and role not in roles:
            continue
        if re.search(name_pattern, name, re.I):
            return ref
    raise StepError("could not find " + name_pattern + " in the snapshot")


def find_completion_selector(listing):
    """Picks the hidden element that looks like the completion signal from the eval listing."""
    best = None
    for m in re.finditer(r"hidden #([\w-]+) <(\w+)> role=(\S+) text=(.*?)(?= \| hidden #|\"?$)", listing):
        ident, _tag, role, text = m.groups()
        score = 0
        if re.search(r"complete|ready|success|done|finished", text + " " + ident, re.I):
            score += 2
        if role in ("status", "alert"):
            score += 1
        if best is None or score > best[0]:
            best = (score, ident)
    if best is None or best[0] == 0:
        raise StepError("no hidden element looked like a completion signal")
    return "#" + best[1]


def build_steps():
    """Returns the ordered list of (narration, command builder) pairs for the scripted run."""

    def open_page(c):
        return "agent-browser open " + c["url"]

    def list_hidden(c):
        return "agent-browser eval '" + HIDDEN_JS + "'"

    def snapshot(c):
        return "agent-browser snapshot -i"

    def fill_project(c):
        ref = find_ref(c["results"][2], r"project", {"textbox"})
        return 'agent-browser fill @%s "%s"' % (ref, c["project"])

    def select_env(c):
        ref = find_ref(c["results"][2], r"environment", {"combobox"})
        return 'agent-browser select @%s "%s"' % (ref, c["environment"])

    def upload(c):
        ref = find_ref(c["results"][2], r"package|upload|file", {"button", "textbox"})
        return 'agent-browser upload @%s "%s"' % (ref, c["path"])

    def start_events(c):
        selector = find_completion_selector(c["results"][1])
        c["selector"] = selector
        return "agent-browser events start --selector '%s'" % selector

    def click_deploy(c):
        ref = find_ref(c["results"][2], r"^deploy$", {"button"})
        return "agent-browser click @" + ref

    return [
        ("I'll start by opening the page.", open_page),
        ("The page loads a form. Before touching it I want to know what will show up when the upload finishes, so I'm listing the elements that are hidden right now.", list_hidden),
        ("Now a snapshot of the interactive elements.", snapshot),
        ("Filling in the project name.", fill_project),
        ("Setting the environment.", select_env),
        ("Attaching the deployment package.", upload),
        ("There is a hidden status region that becomes visible when the upload completes. I'll subscribe to it before submitting, so I don't have to poll.", start_events),
        ("The subscription is in place. Submitting the form now.", click_deploy),
    ]


def submitted_text(c):
    """Builds the message that ends the first turn, right after the click."""
    return "Submitted. I am subscribed to %s and will continue when it fires." % c["selector"]


def final_text(c):
    """Builds the closing summary from the event message that woke the agent."""
    m = EVENT_RE.search(c["event"])
    if not m:
        return "I received an event but could not read it: " + c["event"][:300]
    _id, selector, outcome, page_text = m.groups()
    if not outcome.startswith("element became"):
        return "The subscription on %s %s, so the upload did not report completion." % (selector, outcome)
    shown = "" if page_text == "none" else ' The page says "%s".' % page_text
    return (
        "The upload finished: the event for %s fired (%s).%s "
        "I filled %s for the %s environment, attached the package, subscribed before submitting, and picked this up from the event without polling."
        % (selector, outcome, shown, c["project"], c["environment"])
    )


def next_action(messages):
    """Decides the reply for the current step: a tool call or a closing text."""
    c = parse_request(messages)
    if c["event"]:
        log("event message, final answer")
        return {"text": final_text(c), "command": None}
    steps = build_steps()
    index = len(c["results"])
    try:
        if index < len(steps):
            narration, build = steps[index]
            command = build(c)
            log("step %d: %s" % (index + 1, command))
            return {"text": narration, "command": command}
        c["selector"] = find_completion_selector(c["results"][1])
        log("turn ends after the click")
        return {"text": submitted_text(c), "command": None}
    except StepError as e:
        log("step %d failed: %s" % (index + 1, e))
        return {"text": "I could not continue: " + str(e) + ".", "command": None}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        return

    def send_json(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.split("?")[0] == "/v1/models":
            self.send_json({"object": "list", "data": [{"id": "fake/rehearsal", "object": "model", "owned_by": "fake-gateway"}]})
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path.split("?")[0] != "/v1/chat/completions":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
        except ValueError:
            self.send_error(400)
            return
        model = body.get("model", "fake/rehearsal")
        if not body.get("stream"):
            self.send_json({
                "id": "chatcmpl-" + uuid.uuid4().hex[:12],
                "object": "chat.completion",
                "created": int(time.time()),
                "model": model,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "Rehearsal summary."}, "finish_reason": "stop"}],
            })
            return
        self.stream_reply(model, next_action(body.get("messages", [])))

    def stream_reply(self, model, action):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        cid = "chatcmpl-" + uuid.uuid4().hex[:12]
        created = int(time.time())

        def chunk(delta, finish=None):
            payload = {
                "id": cid,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            }
            self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
            self.wfile.flush()
            time.sleep(0.03)

        chunk({"role": "assistant", "content": ""})
        words = re.findall(r"\S+\s*", action["text"])
        for i in range(0, len(words), 3):
            chunk({"content": "".join(words[i:i + 3])})
        if action["command"]:
            args = json.dumps({"command": action["command"]})
            call = {"index": 0, "id": "call_" + uuid.uuid4().hex[:16], "type": "function", "function": {"name": "agent_browser", "arguments": ""}}
            chunk({"tool_calls": [call]})
            for i in range(0, len(args), 24):
                chunk({"tool_calls": [{"index": 0, "function": {"arguments": args[i:i + 24]}}]})
            chunk({}, "tool_calls")
        else:
            chunk({}, "stop")
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


def main():
    parser = argparse.ArgumentParser(description="OpenAI compatible test double for the events demo rehearsal.")
    parser.add_argument("--port", type=int, default=4950)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    log("listening on http://127.0.0.1:%d (test double, not the demo path)" % args.port)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
