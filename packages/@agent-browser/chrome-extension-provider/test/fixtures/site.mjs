import { createServer } from "node:http";
import { randomBytes } from "node:crypto";
import { realpathSync } from "node:fs";
import { pathToFileURL } from "node:url";

/** A real HTTP login and cross-site iframe, with no browser automation hooks. */
export async function startFixture() {
  const sessions = new Set();
  const requests = [];
  const frame = createServer((_request, response) => {
    response.setHeader("content-type", "text/html; charset=utf-8");
    response.end(`<!doctype html><title>Cross-site frame</title><h2>Cross-site frame</h2><button onclick="document.querySelector('output').textContent='Frame clicked'">Frame action</button><output>Frame ready</output>`);
  });
  await new Promise(resolve => frame.listen(0, "localhost", resolve));
  const frameUrl = `http://localhost:${frame.address().port}`;
  const server = createServer((request, response) => {
    const url = new URL(request.url, "http://127.0.0.1");
    const cookie = (request.headers.cookie || "").split(";").map(item => item.trim()).find(item => item.startsWith("fixture_session="))?.slice("fixture_session=".length);
    const loggedIn = sessions.has(cookie);
    requests.push({ path: url.pathname, loggedIn, userAgent: request.headers["user-agent"] || "" });
    if (url.pathname === "/login" && request.method === "POST") {
      const token = randomBytes(24).toString("hex");
      sessions.add(token);
      response.writeHead(303, { location: "/workspace", "set-cookie": `fixture_session=${token}; Path=/; HttpOnly; SameSite=Lax` });
      response.end(); return;
    }
    if (url.pathname === "/__test/status") {
      response.setHeader("content-type", "application/json"); response.end(JSON.stringify({ requests })); return;
    }
    response.setHeader("content-type", "text/html; charset=utf-8");
    response.setHeader("cache-control", "no-store");
    response.end(`<!doctype html><html lang="en"><meta charset="utf-8"><title>Agent Browser acceptance fixture</title><style>body{font:18px system-ui;max-width:760px;margin:40px auto;padding:24px}label{display:block;margin:20px 0}input,button{font:inherit;padding:8px}iframe{display:block;width:100%;height:170px;border:1px solid #aaa}</style><h1>Agent Browser acceptance fixture</h1>${loggedIn ? `<p id="login-status">Signed in with an existing HttpOnly session</p><label>Unsaved draft <input id="draft" value=""></label><label>Task input <input id="task-input"></label><button onclick="document.querySelector('#result').textContent='Action completed'">Run action</button><p id="result">Ready</p><iframe title="Cross-site test frame" src="${frameUrl}"></iframe><a href="/workspace?visited=1">Navigate in the same tab</a>` : `<p>Guest session</p><form method="post" action="/login"><button>Sign in for test</button></form>`}</html>`);
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  return { url: `http://127.0.0.1:${server.address().port}`, frameUrl,
    close: () => Promise.all([new Promise(resolve => server.close(resolve)), new Promise(resolve => frame.close(resolve))]),
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  const fixture = await startFixture();
  process.stdout.write(JSON.stringify({ url: fixture.url, frameUrl: fixture.frameUrl }) + "\n");
  process.on("SIGTERM", () => void fixture.close().then(() => process.exit(0)));
}
