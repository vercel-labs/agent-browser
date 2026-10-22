#!/usr/bin/env python3
import argparse
import json
import re
import sys
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

EVENT_PREFIX = "[agent-browser event]"
WAKE_RE = re.compile(r"Subscription (\S+) has (\d+) unread event")
HIDDEN_JS = (
    '[...document.querySelectorAll("[hidden]")].map(e => "hidden #" + e.id'
    ' + " <" + e.tagName.toLowerCase() + "> role=" + e.getAttribute("role")'
    ' + " text=" + e.textContent.replace(/[^A-Za-z0-9 .:-]+/g, " ").replace(/ +/g, " ").trim().slice(0, 80)).join(" | ")'
)

def log(message):
    print("[fake-gateway] " + message, file=sys.stderr, flush=True)

def message_text(message):
    content = message.get("content")
    if isinstance(content, str):
        return content
    return " ".join(part.get("text", "") for part in content if isinstance(part, dict)) if isinstance(content, list) else ""

def parse_request(messages):
    users = [(index, message_text(message)) for index, message in enumerate(messages) if message.get("role") == "user"]
    event = users[-1][1] if users and users[-1][1].startswith(EVENT_PREFIX) else None
    prompts = [(index, text) for index, text in users if not text.startswith(EVENT_PREFIX)]
    last_user, text = prompts[-1] if prompts else (-1, "")
    url = re.search(r"https?://[^\s\"'<>)]+", text)
    path = re.search(r"(?<![\w.~])(/[^\s\"'<>`,;()]+)", re.sub(r"https?://[^\s\"'<>)]+", " ", text))
    name = re.search(r"project name[:\s]+[\"']?(.+?)[\"']?(?=\s*(?:,|\.(?:\s|$)|\s+and\s|\s+environment\b|$))", text, re.I)
    environment = re.search(r"\b(production|preview|development)\b", text, re.I)
    return {
        "url": url.group(0).rstrip(".,;:") if url else "http://127.0.0.1:4900",
        "path": path.group(1).rstrip(".,;:") if path else "",
        "project": name.group(1).strip() if name else "Acme Dashboard",
        "environment": environment.group(1).capitalize() if environment else "Production",
        "results": [message_text(message) for message in messages[last_user + 1:] if message.get("role") == "tool"],
        "event": event,
        "event_results": [message_text(message) for message in messages[users[-1][0] + 1:] if message.get("role") == "tool" and message_text(message)] if event else [],
    }

def find_ref(snapshot, name_pattern, roles=None):
    for line in snapshot.splitlines():
        match = re.match(r'\s*-\s*([\w-]+)\s+"([^"]*)"[^\n]*?\bref=(e\d+)', line)
        if match and (not roles or match[1] in roles) and re.search(name_pattern, match[2], re.I):
            return match[3]
    raise ValueError("could not find " + name_pattern + " in the snapshot")

def find_completion_selector(listing):
    candidates = [
        (2 * bool(re.search(r"complete|ready|success|done|finished", text + " " + ident, re.I)) + (role in ("status", "alert")), ident)
        for ident, tag, role, text in re.findall(r"hidden #([\w-]+) <(\w+)> role=(\S+) text=(.*?)(?= \| hidden #|\"?$)", listing)
    ]
    score, ident = max(candidates, key=lambda candidate: candidate[0], default=(0, ""))
    if not score:
        raise ValueError("no hidden element looked like a completion signal")
    return "#" + ident

def next_action(messages):
    context = parse_request(messages)
    if context["event"]:
        wake = WAKE_RE.search(context["event"])
        if not wake:
            log("wake message without a subscription id")
            return {"text": "I received an event wake-up but could not read it: " + context["event"][:300], "command": None}
        subscription, count = wake.groups()
        if not context["event_results"]:
            log("wake received, reading subscription %s" % subscription)
            return {"text": "The subscription has unread events. Reading them now.", "command": "agent-browser events read %s" % subscription}
        log("read result received, final answer")
        return {"text": (
            "The upload finished: subscription %s on the completion signal has %s unread event(s). "
            "I read the subscription, filled %s for the %s environment, attached the package, "
            "subscribed before submitting, and picked this up from the bus without polling."
        ) % (subscription, count, context["project"], context["environment"]), "command": None}
    results = context["results"]
    steps = [
        ("I'll start by opening the page.", lambda: "agent-browser open " + context["url"]),
        ("The page loads a form. Before touching it I want to know what will show up when the upload finishes, so I'm listing the elements that are hidden right now.", lambda: "agent-browser eval '" + HIDDEN_JS + "'"),
        ("Now a snapshot of the interactive elements.", lambda: "agent-browser snapshot -i"),
        ("Filling in the project name.", lambda: 'agent-browser fill @%s "%s"' % (find_ref(results[2], r"project", {"textbox"}), context["project"])),
        ("Setting the environment.", lambda: 'agent-browser select @%s "%s"' % (find_ref(results[2], r"environment", {"combobox"}), context["environment"])),
        ("Attaching the deployment package.", lambda: 'agent-browser upload @%s "%s"' % (find_ref(results[2], r"package|upload|file", {"button", "textbox"}), context["path"])),
        ("There is a hidden status region that becomes visible when the upload completes. I'll subscribe to it before submitting, so I don't have to poll.", lambda: "agent-browser events start --selector '%s' --once" % find_completion_selector(results[1])),
        ("The subscription is in place. Submitting the form now.", lambda: "agent-browser click @" + find_ref(results[2], r"^deploy$", {"button"})),
    ]
    try:
        if len(results) < len(steps):
            narration, build = steps[len(results)]
            command = build()
            log("step %d: %s" % (len(results) + 1, command))
            return {"text": narration, "command": command}
        selector = find_completion_selector(results[1])
        log("turn ends after the click")
        return {"text": "Submitted. I am subscribed to %s and will continue when it fires." % selector, "command": None}
    except ValueError as error:
        log("step %d failed: %s" % (len(results) + 1, error))
        return {"text": "I could not continue: " + str(error) + ".", "command": None}

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
                "id": "chatcmpl-" + uuid.uuid4().hex[:12], "object": "chat.completion", "created": int(time.time()), "model": model,
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
        cid, created = "chatcmpl-" + uuid.uuid4().hex[:12], int(time.time())
        def chunk(delta, finish=None):
            payload = {"id": cid, "object": "chat.completion.chunk", "created": created, "model": model,
                       "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
            self.wfile.flush()
        chunk({"role": "assistant", "content": ""})
        chunk({"content": action["text"]})
        if action["command"]:
            chunk({"tool_calls": [{"index": 0, "id": "call_" + uuid.uuid4().hex[:16], "type": "function", "function": {"name": "agent_browser", "arguments": json.dumps({"command": action["command"]})}}]})
        chunk({}, "tool_calls" if action["command"] else "stop")
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="OpenAI compatible test double for the events demo rehearsal.")
    parser.add_argument("--port", type=int, default=4950)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    log("listening on http://127.0.0.1:%d (test double, not the demo path)" % args.port)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
