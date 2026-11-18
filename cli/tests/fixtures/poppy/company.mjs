// Fixture Company for the browser join (Personal Agent Protocol section 6).
// Usage: node company.mjs <normal|scope|reject|codeurl|offsite|deadport|canonical|upload|query|linklocal|formredirect>. Prints its port, then serves.
// GET /_codes lists issued codes; GET /_hosts (per Host), /_discoveries, /_uploads, /_authorize, /_submits and /_redirects count poppy.json requests, upload-path requests, authorize calls, form submissions and accepted joins.
import http from "node:http";

const mode = process.argv[2] ?? "normal";
const TOKEN = "valid-token";
const codes = new Map();
const sessions = new Map();
let counter = 0;
let submits = 0;
let redirects = 0;
let discoveries = 0;
let uploads = 0;
let authorize = 0;
const hosts = {};

const cookie = (req, name) =>
  (req.headers.cookie ?? "").split(";").map((p) => p.trim().split("=")).find(([k]) => k === name)?.[1];

const document = (origin) => ({
  protocol_version: "1.0",
  organization: { domain: origin.slice("http://".length) },
  web: { browser_session_endpoint: `${origin}/join` },
});

const server = http.createServer((req, res) => {
  let body = "";
  req.on("data", (chunk) => (body += chunk));
  req.on("end", () => {
    const send = (status, headers = {}, text = "") => {
      res.writeHead(status, headers);
      res.end(text);
    };
    const json = (status, value) => send(status, { "content-type": "application/json" }, JSON.stringify(value));
    const html = (text, headers = {}) => send(200, { "content-type": "text/html", ...headers }, text);
    const origin = `http://${req.headers.host}`;
    const route = `${req.method} ${req.url}`;

    if (route === "GET /.well-known/poppy.json") {
      discoveries++;
      hosts[req.headers.host] = (hosts[req.headers.host] ?? 0) + 1;
      // canonical: the document is served from localhost only; 127.0.0.1 redirects there.
      if (mode === "canonical" && req.headers.host.startsWith("127.0.0.1")) {
        return send(301, { location: `http://localhost:${server.address().port}/.well-known/poppy.json` });
      }
      // upload, query: the well-known URL redirects to a document outside the well-known path.
      if (mode === "upload") return send(302, { location: "/uploads/poppy.json" });
      if (mode === "query") return send(302, { location: "/.well-known/poppy.json?v=1" });
      // linklocal: the well-known URL redirects to a link-local address.
      if (mode === "linklocal")
        return send(302, { location: "https://169.254.169.254/.well-known/poppy.json" });
      return json(200, document(origin));
    }
    if (route === "GET /uploads/poppy.json" || route === "GET /.well-known/poppy.json?v=1") {
      uploads++;
      return json(200, document(origin));
    }
    if (route === "GET /_codes") return json(200, [...codes.keys()]);
    if (route === "GET /_submits") return json(200, submits);
    if (route === "GET /_hosts") return json(200, hosts);
    if (route === "GET /_discoveries") return json(200, discoveries);
    if (route === "GET /_uploads") return json(200, uploads);
    if (route === "GET /_authorize") return json(200, authorize);
    if (route === "GET /_redirects") return json(200, redirects);
    if (route === "GET /join") {
      // formredirect: the endpoint sends the tab to a page on another host that carries a code form.
      if (mode === "formredirect")
        return send(302, { location: `http://localhost:${server.address().port}/third-party` });
      const n = counter++;
      const code = `bc_fixture${n}`;
      codes.set(code, { cookie: `jc${n}`, authorized: null, used: false });
      return html(
        `<form method="post" action="/join"><input type="hidden" name="code" value="${code}"><button>Continue</button></form>`,
        { "set-cookie": `__Host-join=jc${n}; Path=/; Max-Age=60; Secure; HttpOnly; SameSite=Strict` },
      );
    }
    if (route === "GET /third-party") {
      return html(
        `<form method="post" action="http://127.0.0.1:${server.address().port}/join"><input type="hidden" name="code" value="bc_third_party"><button>Continue</button></form>`,
      );
    }
    if (route === "POST /join/authorize") {
      authorize++;
      const token = req.headers.authorization?.replace("Bearer ", "");
      if (token && token !== TOKEN) return json(401, { error: "invalid_token" });
      if (token && mode === "scope") return json(403, { error: "insufficient_scope" });
      const entry = codes.get(JSON.parse(body || "{}").code);
      if (!entry || entry.authorized !== null || entry.used) return json(400, { error: "invalid_request" });
      entry.authorized = Boolean(token);
      return send(204);
    }
    if (route === "POST /join") {
      submits++;
      const entry = codes.get(new URLSearchParams(body).get("code"));
      const valid = entry && !entry.used && entry.cookie === cookie(req, "__Host-join");
      if (entry) entry.used = true;
      if (!valid || entry.authorized === null || mode === "reject") return json(400, { error: "invalid_request" });
      const sid = `sid${counter++}`;
      sessions.set(sid, entry.authorized);
      const location =
        mode === "codeurl"
          ? `/?from=${new URLSearchParams(body).get("code")}`
          : mode === "offsite"
            ? `http://localhost:${server.address().port}/`
            : mode === "deadport"
              ? "http://127.0.0.1:1/"
              : "/";
      redirects++;
      return send(303, { location, "set-cookie": `__Host-session=${sid}; Path=/; Secure; HttpOnly; SameSite=Lax` });
    }
    if (req.method === "GET" && req.url.split("?")[0] === "/") {
      const who = sessions.get(cookie(req, "__Host-session"));
      const page = who === true ? "signed in as user_1" : who === false ? "signed out session" : "anonymous visitor";
      return html(`<!doctype html><title>Home</title><p>${page}</p>`);
    }
    send(404, {}, "not found");
  });
});
server.listen(0, "127.0.0.1", () => console.log(server.address().port));
