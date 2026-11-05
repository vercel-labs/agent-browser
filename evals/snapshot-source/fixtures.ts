// Fixture pages and server for the snapshot-source A/B eval.
//
// Every page belongs to a run: /r/<run>/<page>. Pages post their outcomes to ./__event, the
// server records them per run, and task checks read those records. Nothing here trusts what the
// model says it did.
//
// Two hosts reach the same server. localhost and 127.0.0.1 are different sites, so Chrome puts a
// frame from one inside a page from the other into its own renderer process (an out-of-process
// iframe).

export interface RunEvent {
  t: string;
  d: unknown;
  host: string;
}

export interface FixtureContext {
  run: string;
  port: number;
  /** Absolute URL of a page for this run, on localhost by default. */
  url(page: string, host?: "localhost" | "127.0.0.1"): string;
}

const HEAD = `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<style>body{font:16px system-ui;margin:24px;max-width:900px}label{display:block;margin:10px 0 4px}
input,select,textarea,button{font:inherit;padding:6px 8px}fieldset{margin:12px 0}iframe{width:100%;border:1px solid #888}
.btn{display:inline-block;padding:6px 12px;border:1px solid #444;border-radius:4px;cursor:pointer;margin:4px;user-select:none}
.ghost{width:0;height:0;padding:0;border:0;font-size:0;overflow:hidden}</style>
<script>
const EV=(t,d)=>{const p=document.createElement('p');p.textContent='Recorded: '+t;document.body.append(p);return fetch('./__event',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({t,d})})};
const form=(f)=>Object.fromEntries(new FormData(f).entries());
</script>`;

const page = (title: string, body: string, script = "") =>
  `${HEAD}<title>${title}</title><h1>${title}</h1>${body}${script ? `<script>${script}</script>` : ""}`;

type PageFn = (ctx: FixtureContext) => string;

/** Services of the iframe-heavy page: the key of each one rotates from inside its own frame. */
const SERVICES: [string, "localhost" | "127.0.0.1"][] = [
  ["auth", "localhost"],
  ["ledger", "127.0.0.1"],
  ["search", "localhost"],
  ["media", "127.0.0.1"],
  ["mailer", "localhost"],
  ["cache", "127.0.0.1"],
];

export const pages: Record<string, PageFn> = {
  // ---- forms -------------------------------------------------------------------------------
  signup: () =>
    page(
      "Create your account",
      `<form id="f"><label for="n">Full name</label><input id="n" name="name">
<label for="e">Email</label><input id="e" name="email" type="email">
<label for="p">Plan</label><select id="p" name="plan"><option value="free">Free</option><option value="pro">Pro</option><option value="team">Team</option></select>
<label><input type="checkbox" name="newsletter"> Subscribe to the newsletter</label>
<p><button type="submit">Create account</button></p></form>`,
      `f.onsubmit=e=>{e.preventDefault();const d=form(f);EV('submit',{name:d.name,email:d.email,plan:d.plan,newsletter:d.newsletter==='on'});document.title='done'}`,
    ),

  shipping: () =>
    page(
      "Shipping",
      `<form id="f"><fieldset><legend>Delivery speed</legend>
<label><input type="radio" name="speed" value="standard"> Standard (5-7 days)</label>
<label><input type="radio" name="speed" value="express"> Express (2 days)</label>
<label><input type="radio" name="speed" value="overnight"> Overnight</label></fieldset>
<label for="z">Postal code</label><input id="z" name="zip">
<label for="i">Delivery instructions</label><textarea id="i" name="notes"></textarea>
<p><button type="submit">Place order</button></p></form>`,
      `f.onsubmit=e=>{e.preventDefault();EV('submit',form(f))}`,
    ),

  wizard: () =>
    page(
      "Create a workspace",
      `<form id="f"><div id="s1"><label for="e">Work email</label><input id="e" name="email" type="email">
<p><button type="button" id="next">Next</button></p></div>
<div id="s2" hidden><label for="pw">Password</label><input id="pw" name="password" type="password">
<label><input type="checkbox" name="agree"> I agree to the terms</label>
<p><button type="submit">Create workspace</button></p></div></form>`,
      `next.onclick=()=>{s1.hidden=true;s2.hidden=false};f.onsubmit=e=>{e.preventDefault();const d=form(f);EV('submit',{email:d.email,password:d.password,agree:d.agree==='on'})}`,
    ),

  route: () =>
    page(
      "Find flights",
      `<form id="f"><input name="from" placeholder="From city"> <input name="to" placeholder="To city">
<input name="pax" type="number" min="1" placeholder="Passengers"> <button type="submit"><b>Search flights</b></button></form>`,
      `f.onsubmit=e=>{e.preventDefault();EV('submit',form(f))}`,
    ),

  terms: () =>
    page(
      "Register",
      `<form id="f"><label for="n">Name</label><input id="n" name="name">
<label><input type="checkbox" name="accept" id="acc"> I accept the terms</label>
<label><input type="checkbox" name="promo"> Send me promotions</label>
<p><button type="submit" id="go" disabled>Register</button></p></form>`,
      `acc.onchange=()=>{go.disabled=!acc.checked};f.onsubmit=e=>{e.preventDefault();const d=form(f);EV('submit',{name:d.name,accept:d.accept==='on',promo:d.promo==='on'})}`,
    ),

  // ---- iframes -----------------------------------------------------------------------------
  "frame-same": (c) =>
    page("Shipment review", `<p>Review the shipment below.</p><iframe src="${c.url("panel")}" height="160"></iframe>`),
  panel: () =>
    page("Shipment panel", `<div class="btn" role="button" tabindex="0" id="b">Confirm shipment</div>`, `b.onclick=()=>EV('confirm',1)`),

  "frame-deep": (c) =>
    page("Order notes", `<p>Notes live two frames down.</p><iframe src="${c.url("deep1")}" height="260"></iframe>`),
  deep1: (c) => page("Level one", `<iframe src="${c.url("deep2")}" height="180"></iframe>`),
  deep2: () =>
    page(
      "Level two",
      `<label for="n">Note</label><input id="n"> <button id="s">Save note</button>`,
      `s.onclick=()=>EV('note',n.value)`,
    ),

  "frame-cross": (c) =>
    page("Checkout", `<p>Pay securely.</p><iframe src="${c.url("pay", "127.0.0.1")}" height="200"></iframe>`),
  pay: () =>
    page(
      "Secure payment",
      `<label for="c">Card number</label><input id="c" autocomplete="off"> <button id="p">Pay $20</button>`,
      `p.onclick=()=>EV('pay',c.value)`,
    ),

  "frame-cross-deep": (c) =>
    page("Approvals", `<p>An approval widget is embedded below.</p><iframe src="${c.url("cross-mid", "127.0.0.1")}" height="300"></iframe>`),
  "cross-mid": (c) =>
    page("Widget host", `<p>Hosted widget.</p><iframe src="${c.url("cross-leaf")}" height="140"></iframe>`),
  "cross-leaf": () =>
    page("Approval", `<button id="a">Approve request</button>`, `a.onclick=()=>EV('approve',1)`),

  // ---- held-out pages (added before the v2 implementation) -----------------------------------
  "frame-form": (c) =>
    page("Contact desk", `<p>The contact form is embedded below.</p><iframe src="${c.url("contact-form")}" height="340"></iframe>`),
  "contact-form": () =>
    page(
      "Contact form",
      `<form id="f"><label for="fn">Full name</label><input id="fn" name="name"><label for="fm">Email</label><input id="fm" name="email" type="email">
<label for="ft">Topic</label><select id="ft" name="topic"><option value="">Choose a topic</option><option value="billing">Billing</option><option value="support">Support</option><option value="sales">Sales</option></select>
<p><button type="submit">Send request</button></p></form>`,
      `f.onsubmit=e=>{e.preventDefault();EV('contact',form(f))}`,
    ),

  "frame-deep-b": (c) =>
    page("Print settings", `<p>The size picker is nested in two frames.</p><iframe src="${c.url("deep-b1")}" height="300"></iframe>`),
  "deep-b1": (c) => page("Layout", `<iframe src="${c.url("deep-b2")}" height="200"></iframe>`),
  "deep-b2": () =>
    page(
      "Size",
      `<label for="sz">Paper size</label><select id="sz"><option value="s">Small</option><option value="m">Medium</option><option value="l">Large</option></select> <button id="ap">Apply size</button>`,
      `ap.onclick=()=>EV('size',sz.value)`,
    ),

  "frame-cross-button": (c) =>
    page("Newsletter", `<p>Get weekly updates.</p><iframe src="${c.url("subscribe-widget", "127.0.0.1")}" height="140"></iframe>`),
  "subscribe-widget": () =>
    page("Updates", `<p>Weekly product news.</p><button id="b">Subscribe</button>`, `b.onclick=()=>EV('subscribe',1)`),

  "frame-heavy": (c) =>
    page(
      "Key management",
      `<p>Rotate API keys per service.</p>${SERVICES.map(([n, h]) => `<iframe title="${n}" src="${c.url(`svc-${n}`, h)}" height="110"></iframe>`).join("")}`,
    ),

  survey: () =>
    page(
      "Visit feedback",
      `<form id="f"><fieldset><legend>How was your visit?</legend>
<label><input type="radio" name="rating" value="good"> Good</label><label><input type="radio" name="rating" value="ok"> Okay</label><label><input type="radio" name="rating" value="bad"> Poor</label></fieldset>
<label for="vt">Visit type</label><select id="vt" name="visit"><option>In store</option><option>Online</option><option>Phone</option></select>
<label for="cm">Comments</label><textarea id="cm" name="comments"></textarea>
<label><input type="checkbox" name="contact"> You may contact me</label>
<p><button type="submit">Send feedback</button></p></form>`,
      `f.onsubmit=e=>{e.preventDefault();const d=form(f);EV('submit',{rating:d.rating,visit:d.visit,comments:d.comments,contact:d.contact==='on'})}`,
    ),

  "files-menu": () =>
    page(
      "Files",
      `<ul id="files">${["report.pdf", "notes.txt", "budget.xlsx"]
        .map((f) => `<li data-f="${f}"><span>${f}</span> <span class="btn" role="button" tabindex="0" aria-label="Actions for ${f}">&#8943;</span></li>`)
        .join("")}</ul>
<div id="mnu" hidden><div class="btn" role="menuitem" tabindex="0" id="mr">Rename</div><div class="btn" role="menuitem" tabindex="0" id="md">Delete</div></div>
<div id="mdl" role="dialog" aria-label="Rename file" hidden><label for="nn">New name</label><input id="nn"> <button id="sv">Save</button> <button id="cn">Cancel</button></div>`,
      `let cur=null;document.querySelectorAll('[aria-label^="Actions"]').forEach(b=>b.onclick=()=>{cur=b.parentElement.dataset.f;b.parentElement.append(mnu);mnu.hidden=false});
mr.onclick=()=>{mnu.hidden=true;mdl.hidden=false;nn.value=cur};sv.onclick=()=>{EV('rename',{from:cur,to:nn.value});mdl.hidden=true};cn.onclick=()=>{mdl.hidden=true};md.onclick=()=>{EV('delete',cur);mnu.hidden=true}`,
    ),

  // ---- SPA and div-button apps -------------------------------------------------------------
  "cart-spa": () =>
    page(
      "Fruit stand",
      `<div id="app"><div class="btn" id="a1">Add apple</div><div class="btn" id="a2">Add pear</div>
<p>In cart: <span id="count">nothing</span></p><div class="btn" id="co">Checkout</div></div>`,
      `const cart={apple:0,pear:0};const show=()=>count.textContent=Object.entries(cart).filter(x=>x[1]).map(x=>x[0]+' x'+x[1]).join(', ')||'nothing';
a1.onclick=()=>{cart.apple++;show()};a2.onclick=()=>{cart.pear++;show()};co.onclick=()=>EV('checkout',{...cart})`,
    ),

  "settings-spa": () =>
    page(
      "Settings",
      `<div id="tabs"><div role="tab" class="btn" id="t1" tabindex="0">General</div><div role="tab" class="btn" id="t2" tabindex="0">Notifications</div><div role="tab" class="btn" id="t3" tabindex="0">Billing</div></div>
<div id="p1">General settings.</div>
<div id="p2" hidden><div role="switch" aria-checked="false" tabindex="0" id="sw" class="btn">Email alerts: off</div>
<div class="btn" id="save">Save changes</div></div><div id="p3" hidden>Billing details.</div>`,
      `const show=n=>['p1','p2','p3'].forEach((p,i)=>document.getElementById(p).hidden=i!==n);
t1.onclick=()=>show(0);t2.onclick=()=>show(1);t3.onclick=()=>show(2);
sw.onclick=()=>{const on=sw.getAttribute('aria-checked')!=='true';sw.setAttribute('aria-checked',String(on));sw.textContent='Email alerts: '+(on?'on':'off')};
save.onclick=()=>EV('save',{email:sw.getAttribute('aria-checked')==='true'})`,
    ),

  overlay: () =>
    page(
      "Newsletter",
      `<p><button id="sub">Subscribe</button></p>
<div id="dim" style="position:fixed;inset:0;background:rgba(0,0,0,.5);z-index:10"></div>
<div id="cookie" role="dialog" aria-label="Cookies" style="position:fixed;left:10%;right:10%;bottom:10%;background:#fff;padding:16px;border:1px solid #000;z-index:11">
<p>We use cookies.</p><button id="acc">Accept all</button> <button id="rej">Reject all</button></div>`,
      `const close=()=>{dim.remove();cookie.remove()};acc.onclick=()=>{EV('cookies','accept');close()};rej.onclick=()=>{EV('cookies','reject');close()};sub.onclick=()=>EV('subscribe',1)`,
    ),

  dropdown: () =>
    page(
      "Catalog",
      `<div class="btn" id="dd">Sort by: Relevance</div><div id="menu" hidden style="margin-left:12px">
<div class="btn" data-v="relevance">Relevance</div><div class="btn" data-v="price-asc">Price: low to high</div><div class="btn" data-v="newest">Newest</div></div>`,
      `dd.onclick=()=>{menu.hidden=!menu.hidden};menu.onclick=e=>{const v=e.target.dataset.v;if(v){EV('sort',v);dd.textContent='Sort by: '+e.target.textContent;menu.hidden=true}}`,
    ),

  // ---- heavy pages -------------------------------------------------------------------------
  "docs-heavy": (c) => {
    const side = Array.from({ length: 300 }, (_, i) => `<li><a href="${c.url("docs-topic")}#s${i}">Reference topic ${i}</a></li>`).join("");
    const sections = Array.from({ length: 40 }, (_, s) => {
      const links = Array.from({ length: 10 }, (_, i) => `<a href="${c.url("docs-topic")}#${s}-${i}">See guide ${s}.${i}</a>`).join(" | ");
      const extra = s === 27 ? ` | <a href="${c.url("docs-retry")}">Configure retry policy</a> | <a href="${c.url("docs-topic")}">Configure retry policies (legacy)</a> | <a href="${c.url("docs-topic")}">Configure retry timeouts</a>` : "";
      return `<h2>Section ${s}</h2><p>${links}${extra}</p>`;
    }).join("");
    return page("Platform documentation", `<nav><ul>${side}</ul></nav><main>${sections}</main>`);
  },
  "docs-topic": () => page("Topic", "<p>Not the page you wanted.</p>"),
  "docs-retry": () => page("Retry policy", "<p>Retries configured.</p>"),

  orders: () => {
    const rows = Array.from({ length: 300 }, (_, i) => {
      const id = `ORD-${7000 + i}`;
      return `<tr><td>${id}</td><td>Customer ${i}</td><td>shipped</td><td><button aria-label="Archive ${id}" data-id="${id}">Archive</button></td></tr>`;
    }).join("");
    return page(
      "Orders",
      `<table><thead><tr><th>Order</th><th>Customer</th><th>Status</th><th></th></tr></thead><tbody id="tb">${rows}</tbody></table>`,
      `tb.onclick=e=>{const b=e.target.closest('button');if(b)EV('archive',b.dataset.id)}`,
    );
  },

  products: () => {
    const names = Array.from({ length: 600 }, (_, i) => (i === 431 ? "Quartz Desk Lamp" : `Catalog item ${i}`));
    const cards = names
      .map(
        (n) =>
          `<div class="card"><h3>${n}</h3><button class="ghost" aria-label="Add ${n} to cart" data-n="ghost">Add to cart</button><button aria-label="Add ${n} to cart" data-n="${n}">Add to cart</button></div>`,
      )
      .join("");
    return page("Shop", `<div id="grid">${cards}</div>`, `grid.onclick=e=>{const b=e.target.closest('button');if(b)EV('add',b.dataset.n)}`);
  },

  // ---- traps -------------------------------------------------------------------------------
  ghost: () =>
    page(
      "Contact us",
      `<label for="e">Your email</label><input id="e"> <button class="ghost" id="g">Send</button> <button id="s">Send</button>`,
      `g.onclick=()=>EV('ghost',1);s.onclick=()=>EV('send',e.value)`,
    ),

  accordion: () =>
    page(
      "Account help",
      `<div><button aria-expanded="false" id="h1">Account</button><div id="p1" hidden>Manage your profile.</div></div>
<div><button aria-expanded="false" id="h2">Billing</button><div id="p2" hidden><a href="#" id="inv">Download invoice</a></div></div>
<div><button aria-expanded="false" id="h3">Support</button><div id="p3" hidden>Contact us.</div></div>`,
      `[1,2,3].forEach(n=>{const h=document.getElementById('h'+n),p=document.getElementById('p'+n);h.onclick=()=>{p.hidden=!p.hidden;h.setAttribute('aria-expanded',String(!p.hidden))}});
inv.onclick=e=>{e.preventDefault();EV('invoice',1)}`,
    ),

  covered: () =>
    page(
      "Project settings",
      `<p><button id="del" style="padding:12px 24px">Delete project</button></p>
<div id="pop" role="dialog" aria-label="Newsletter" style="position:absolute;left:0;top:70px;width:420px;height:120px;background:#ffd;border:1px solid #000;padding:12px;z-index:5">
Join our newsletter! <button id="no">No thanks</button> <button id="yes">Sign me up</button></div>`,
      `no.onclick=()=>{EV('popup','dismissed');pop.remove()};yes.onclick=()=>EV('newsletter',1);del.onclick=()=>EV('delete',1)`,
    ),

  shadow: () =>
    page(
      "Coupons",
      `<x-card></x-card>`,
      `customElements.define('x-card',class extends HTMLElement{constructor(){super();const r=this.attachShadow({mode:'open'});
r.innerHTML='<label>Coupon <input aria-label="Coupon code"></label> <button>Activate</button>';
r.querySelector('button').onclick=()=>EV('activate',{coupon:r.querySelector('input').value})}})`,
    ),
};

for (const [name] of SERVICES) {
  pages[`svc-${name}`] = () => page(`Service ${name}`, `<p>Status: healthy</p><button id="r">Rotate key</button>`, `r.onclick=()=>EV('rotate','${name}')`);
}

export interface FixtureServer {
  port: number;
  events(run: string): RunEvent[];
  stop(): void;
}

export function startFixtureServer(): FixtureServer {
  const log = new Map<string, RunEvent[]>();
  const server = Bun.serve({
    hostname: "0.0.0.0",
    port: 0,
    async fetch(req) {
      const url = new URL(req.url);
      const m = url.pathname.match(/^\/r\/([^/]+)\/(.+)$/);
      if (!m) return new Response("not found", { status: 404 });
      const [, run, name] = m;
      if (name === "__event" && req.method === "POST") {
        const body = (await req.json()) as { t: string; d: unknown };
        const list = log.get(run) ?? [];
        list.push({ t: body.t, d: body.d, host: url.hostname });
        log.set(run, list);
        return new Response("ok");
      }
      const fn = pages[name];
      if (!fn) return new Response("not found", { status: 404 });
      const ctx: FixtureContext = {
        run,
        port: server.port as number,
        url: (p, host = "localhost") => `http://${host}:${server.port}/r/${run}/${p}`,
      };
      return new Response(fn(ctx), { headers: { "content-type": "text/html; charset=utf-8" } });
    },
  });
  return {
    port: server.port as number,
    events: (run) => log.get(run) ?? [],
    stop: () => server.stop(true),
  };
}

