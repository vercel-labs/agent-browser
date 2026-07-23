import assert from "node:assert/strict";
import test from "node:test";

import {
  AgentBrowserCommandError,
  createAgentBrowserCommandResult,
  throwIfCommandFailed,
} from "../dist/index.js";

test("createAgentBrowserCommandResult defaults to exit 0 and empty streams", () => {
  const result = createAgentBrowserCommandResult({ command: "snapshot" });
  assert.equal(result.exitCode, 0);
  assert.equal(result.stdout, "");
  assert.equal(result.stderr, "");
  assert.equal(result.json, null);
});

test("createAgentBrowserCommandResult parses JSON stdout", () => {
  const result = createAgentBrowserCommandResult({
    command: "snapshot",
    stdout: '{"tabs":[]}',
  });
  assert.deepEqual(result.json, { tabs: [] });
});

test("createAgentBrowserCommandResult returns null json for invalid JSON", () => {
  const result = createAgentBrowserCommandResult({
    command: "snapshot",
    stdout: "not json",
  });
  assert.equal(result.json, null);
});

test("throwIfCommandFailed passes through on success", () => {
  const result = createAgentBrowserCommandResult({
    command: "open",
    exitCode: 0,
    stdout: "ok",
  });
  const returned = throwIfCommandFailed(result);
  assert.equal(returned, result);
});

test("throwIfCommandFailed throws AgentBrowserCommandError on non-zero exit", () => {
  const result = createAgentBrowserCommandResult({
    command: "open",
    exitCode: 1,
    stderr: "something broke",
  });
  assert.throws(
    () => throwIfCommandFailed(result),
    (err) => {
      assert.ok(err instanceof AgentBrowserCommandError);
      assert.equal(err.exitCode, 1);
      assert.equal(err.command, "open");
      assert.match(err.message, /agent-browser command failed/);
      assert.match(err.message, /something broke/);
      return true;
    },
  );
});

test("AgentBrowserCommandError includes stderr detail", () => {
  const err = new AgentBrowserCommandError({
    command: "close",
    exitCode: 2,
    stderr: "  connection refused  ",
    stdout: "",
  });
  assert.equal(err.exitCode, 2);
  assert.equal(err.name, "AgentBrowserCommandError");
  assert.match(err.message, /connection refused/);
});

test("AgentBrowserCommandError falls back to stdout when stderr is empty", () => {
  const err = new AgentBrowserCommandError({
    command: "close",
    exitCode: 1,
    stderr: "",
    stdout: "timeout exceeded",
  });
  assert.match(err.message, /timeout exceeded/);
});

test("AgentBrowserCommandError falls back to exit code when both streams are empty", () => {
  const err = new AgentBrowserCommandError({
    command: "close",
    exitCode: 127,
    stderr: "",
    stdout: "",
  });
  assert.match(err.message, /exit 127/);
});