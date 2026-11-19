// Test authorizer: performs step 2 of the browser join against the fixture Company.
// Reads one JSON line on stdin and prints one JSON line.
// POPPY_TEST_TOKEN: send this access token.
// POPPY_TEST_DELAY_MS: wait this long before answering.
import { createInterface } from "node:readline";

const [line] = await new Promise((resolve) => {
  const lines = [];
  const rl = createInterface({ input: process.stdin });
  rl.on("line", (l) => lines.push(l));
  rl.on("close", () => resolve(lines));
});
const input = JSON.parse(line);
await new Promise((resolve) => setTimeout(resolve, Number(process.env.POPPY_TEST_DELAY_MS ?? 0)));

const headers = {
  "content-type": "application/json",
  "poppy-version": input.poppy_version,
  "poppy-session": "test-session",
};
const token = process.env.POPPY_TEST_TOKEN;
if (token) headers.authorization = `Bearer ${token}`;

const response = await fetch(input.authorize_url, {
  method: "POST",
  headers,
  body: JSON.stringify({ code: input.code }),
  redirect: "manual",
});
if (response.status === 204) {
  console.log(JSON.stringify({ signed_in: Boolean(token) }));
} else {
  const body = await response.json().catch(() => ({}));
  console.log(JSON.stringify({ error: body.error ?? "authorizer_failed" }));
  process.exit(1);
}
