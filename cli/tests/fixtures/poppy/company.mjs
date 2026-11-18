// Fixture Company for the browser join (Personal Agent Protocol section 6).
// Usage: node company.mjs <normal|scope|reject>. Prints its port, then serves.
// GET /_codes lists the codes issued so far.
import http from "node:http";

const mode = process.argv[2] ?? "normal";
const TOKEN = "valid-token";
const codes = new Map();
const sessions = new Map();
let counter = 0;

const cookie = (req, name) =>
  (req.headers.cookie ?? "").split(";").map((p) => p.trim().split("=")).find(([k]) => k === name)?.[1];

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
    const origin = `http://127.0.0.1:${server.address().port}`;
    const route = `${req.method} ${req.url}`;

    if (route === "GET /.well-known/poppy.json") {
      return json(200, {
        protocol_version: "1.0",
        organization: { domain: origin.slice("http://".length) },
        web: { browser_session_endpoint: `${origin}/join` },
      });
    }
    if (route === "GET /_codes") return json(200, [...codes.keys()]);
    if (route === "GET /join") {
      const n = counter++;
      const code = `bc_fixture${n}`;
      codes.set(code, { cookie: `jc${n}`, authorized: null, used: false });
      return html(
        `<form method="post" action="/join"><input type="hidden" name="code" value="${code}"><button>Continue</button></form>`,
        { "set-cookie": `__Host-join=jc${n}; Path=/; Max-Age=60; Secure; HttpOnly; SameSite=Strict` },
      );
    }
    if (route === "POST /join/authorize") {
      const token = req.headers.authorization?.replace("Bearer ", "");
      if (token && token !== TOKEN) return json(401, { error: "invalid_token" });
      if (token && mode === "scope") return json(403, { error: "insufficient_scope" });
      const entry = codes.get(JSON.parse(body || "{}").code);
      if (!entry || entry.authorized !== null || entry.used) return json(400, { error: "invalid_request" });
      entry.authorized = Boolean(token);
      return send(204);
    }
    if (route === "POST /join") {
      const entry = codes.get(new URLSearchParams(body).get("code"));
      const valid = entry && !entry.used && entry.cookie === cookie(req, "__Host-join");
      if (entry) entry.used = true;
      if (!valid || entry.authorized === null || mode === "reject") return json(400, { error: "invalid_request" });
      const sid = `sid${counter++}`;
      sessions.set(sid, entry.authorized);
      return send(303, { location: "/", "set-cookie": `__Host-session=${sid}; Path=/; Secure; HttpOnly; SameSite=Lax` });
    }
    if (route === "GET /") {
      const who = sessions.get(cookie(req, "__Host-session"));
      const page = who === true ? "signed in as user_1" : who === false ? "signed out session" : "anonymous visitor";
      return html(`<!doctype html><title>Home</title><p>${page}</p>`);
    }
    send(404, {}, "not found");
  });
});
server.listen(0, "127.0.0.1", () => console.log(server.address().port));
