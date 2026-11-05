// Public-page tasks of the v3 snapshot-source eval. Written and committed before any v3 run.
//
// Success is decided by the harness, never by the model:
// - navigation: the final URL (path, query or hash);
// - forms: the echoed payload or the query string the site produced;
// - page state: read back from the page with a direct DOM query after the run;
// - extraction: the model's final answer compared with ground truth the harness computes at run
//   time with an independent method (an HTTP fetch of the page, its source or an API).
// Rules for public pages: no login, no account, no purchase, no submission to a real business or
// person, no personal data. Sites are visited sequentially with a delay.

export type PublicCategory = "nav" | "extract" | "form" | "iframe" | "spa" | "heavy";

export interface PublicCtx {
  url: string;
  /** Final answer text of the model (done summary or last message). */
  answer: string;
  /** Ground truth computed by the harness before the run (and after it, as `truthAfter`). */
  truth: any;
  truthAfter: any;
  /** Evaluate a JS expression in the main frame of the task page and return its JSON value. */
  ev(js: string): Promise<any>;
}

export interface PublicTask {
  id: string;
  site: string;
  category: PublicCategory;
  url: string;
  prompt: string;
  /** Ground truth by an independent method. Throws when it cannot be computed (run is invalid). */
  truth?(): Promise<any>;
  check(ctx: PublicCtx): Promise<boolean> | boolean;
}

const UA = "Mozilla/5.0 (compatible; agent-browser-eval/1.0)";

const cache = new Map<string, { at: number; text: string }>();
/** Plain HTTP fetch, cached for ten minutes so repeated runs stay polite. */
export async function fetchText(url: string): Promise<string> {
  const hit = cache.get(url);
  if (hit && Date.now() - hit.at < 10 * 60_000) return hit.text;
  const res = await fetch(url, { headers: { "user-agent": UA, accept: "text/html,application/json" } });
  if (!res.ok) throw new Error(`truth fetch ${url} -> ${res.status}`);
  const text = await res.text();
  cache.set(url, { at: Date.now(), text });
  return text;
}

const norm = (s: string) => s.toLowerCase().replace(/[\s ]+/g, " ").replace(/[*_`"']/g, "").trim();
const has = (answer: string, needle: string) => norm(answer).includes(norm(needle));
const decode = (s: string) => s.replace(/&amp;/g, "&").replace(/&quot;/g, '"').replace(/&#0?39;/g, "'").replace(/&lt;/g, "<").replace(/&gt;/g, ">");
const strip = (s: string) => decode(s.replace(/<[^>]*>/g, "")).replace(/\s+/g, " ").trim();
const u = (url: string) => new URL(url);
const MONTHS = ["", "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

const urlIs = (host: string, path: string) => (ctx: PublicCtx) => {
  const x = u(ctx.url);
  return x.hostname === host && x.pathname === path;
};

export const publicTasks: PublicTask[] = [
  // ---------------- forms on testing and echo sites ----------------
  {
    id: "pub-httpbin-pizza",
    site: "httpbin.org",
    category: "form",
    url: "https://httpbin.org/forms/post",
    prompt:
      "Fill the order form with customer name Ada Lovelace, telephone 555-0100, email ada@example.com, a large pizza with bacon and onion, delivery time 18:30 and the delivery instructions \"Ring twice\". Submit it.",
    async check({ ev, url }) {
      if (u(url).pathname !== "/post") return false;
      const body = await ev("document.body.innerText");
      let j: any;
      try {
        j = JSON.parse(String(body));
      } catch {
        return false;
      }
      const f = j.form ?? {};
      const top = ([] as string[]).concat(f.topping ?? []).sort();
      return f.custname === "Ada Lovelace" && f.custtel === "555-0100" && f.custemail === "ada@example.com" && f.size === "large" && JSON.stringify(top) === '["bacon","onion"]' && f.delivery === "18:30" && f.comments === "Ring twice";
    },
  },
  {
    id: "pub-httpbin-pizza-small",
    site: "httpbin.org",
    category: "form",
    url: "https://httpbin.org/forms/post",
    prompt: "Order a small pizza with mushroom only for the customer Grace Hopper, telephone 555-0199, email grace@example.com. Leave the delivery time and instructions empty. Submit the form.",
    async check({ ev, url }) {
      if (u(url).pathname !== "/post") return false;
      let j: any;
      try {
        j = JSON.parse(String(await ev("document.body.innerText")));
      } catch {
        return false;
      }
      const f = j.form ?? {};
      const top = ([] as string[]).concat(f.topping ?? []);
      return f.custname === "Grace Hopper" && f.custtel === "555-0199" && f.custemail === "grace@example.com" && f.size === "small" && top.length === 1 && top[0] === "mushroom" && !f.delivery && !f.comments;
    },
  },
  {
    id: "pub-demo-webform",
    site: "web-form-demo",
    category: "form",
    url: "https://www.selenium.dev/selenium/web/web-form.html",
    prompt: "In the web form, type \"hello world\" in the text input, type \"second line\" in the textarea, choose Two in the dropdown (select) and submit the form.",
    check({ url }) {
      const x = u(url);
      return x.pathname.endsWith("/submitted-form.html") && x.searchParams.get("my-text") === "hello world" && x.searchParams.get("my-textarea") === "second line" && x.searchParams.get("my-select") === "2";
    },
  },
  {
    id: "pub-demo-webform-checks",
    site: "web-form-demo",
    category: "form",
    url: "https://www.selenium.dev/selenium/web/web-form.html",
    prompt: "In the web form, make sure both checkboxes are checked, type \"abc\" in the text input and submit the form.",
    check({ url }) {
      const x = u(url);
      const q = x.searchParams;
      return x.pathname.endsWith("/submitted-form.html") && q.get("my-text") === "abc" && q.getAll("my-check").length === 2;
    },
  },
  {
    id: "pub-herokuapp-dropdown",
    site: "the-internet.herokuapp.com",
    category: "form",
    url: "https://the-internet.herokuapp.com/dropdown",
    prompt: "Choose Option 2 in the dropdown.",
    async check({ ev }) {
      return (await ev("document.querySelector('#dropdown').value")) === "2";
    },
  },
  {
    id: "pub-herokuapp-checkboxes",
    site: "the-internet.herokuapp.com",
    category: "form",
    url: "https://the-internet.herokuapp.com/checkboxes",
    prompt: "Make sure the first checkbox is checked and the second checkbox is unchecked.",
    async check({ ev }) {
      return (await ev("JSON.stringify([...document.querySelectorAll('#checkboxes input')].map(c=>c.checked))")) === "[true,false]";
    },
  },
  {
    id: "pub-herokuapp-add-remove",
    site: "the-internet.herokuapp.com",
    category: "form",
    url: "https://the-internet.herokuapp.com/add_remove_elements/",
    prompt: "Add four elements, then delete exactly one of them, so three Delete buttons remain.",
    async check({ ev }) {
      return Number(await ev("document.querySelectorAll('#elements .added-manually').length")) === 3;
    },
  },

  // ---------------- navigation and finding ----------------
  {
    id: "pub-wiki-search-ada",
    site: "wikipedia.org",
    category: "nav",
    url: "https://en.wikipedia.org/wiki/Main_Page",
    prompt: "Find the Wikipedia article about Ada Lovelace and open it.",
    check: urlIs("en.wikipedia.org", "/wiki/Ada_Lovelace"),
  },
  {
    id: "pub-pydocs-loads",
    site: "docs.python.org",
    category: "nav",
    url: "https://docs.python.org/3/library/json.html",
    prompt: "Jump to the documentation entry of json.loads on this page, so that the URL ends with the anchor of that entry.",
    check: ({ url }) => u(url).pathname.endsWith("/library/json.html") && u(url).hash === "#json.loads",
  },
  {
    id: "pub-reactdev-props",
    site: "react.dev",
    category: "nav",
    url: "https://react.dev/learn",
    prompt: "Open the page \"Passing Props to a Component\".",
    check: urlIs("react.dev", "/learn/passing-props-to-a-component"),
  },
  {
    id: "pub-rustbook-ownership",
    site: "doc.rust-lang.org",
    category: "nav",
    url: "https://doc.rust-lang.org/book/",
    prompt: "Open the chapter \"Understanding Ownership\".",
    check: urlIs("doc.rust-lang.org", "/book/ch04-00-understanding-ownership.html"),
  },
  {
    id: "pub-vue-reactivity",
    site: "vuejs.org",
    category: "nav",
    url: "https://vuejs.org/guide/introduction.html",
    prompt: "Open the guide page \"Reactivity Fundamentals\".",
    check: urlIs("vuejs.org", "/guide/essentials/reactivity-fundamentals.html"),
  },
  {
    id: "pub-example-learn-more",
    site: "example.com",
    category: "nav",
    url: "https://example.com/",
    prompt: "Follow the \"Learn more\" link on this page.",
    check: ({ url }) => u(url).hostname.endsWith("iana.org"),
  },
  {
    id: "pub-hn-newest",
    site: "news.ycombinator.com",
    category: "nav",
    url: "https://news.ycombinator.com/",
    prompt: "Open the list of the newest submissions.",
    check: urlIs("news.ycombinator.com", "/newest"),
  },
  {
    id: "pub-books-travel",
    site: "toscrape.com",
    category: "nav",
    url: "https://books.toscrape.com/",
    prompt: "Open the Travel category of the bookstore.",
    check: urlIs("books.toscrape.com", "/catalogue/category/books/travel_2/index.html"),
  },
  {
    id: "pub-quotes-js-page3",
    site: "toscrape.com",
    category: "nav",
    url: "https://quotes.toscrape.com/js/",
    prompt: "Go to page 3 of the quotes by using the Next link twice.",
    check: urlIs("quotes.toscrape.com", "/js/page/3/"),
  },

  // ---------------- heavy pages ----------------
  {
    id: "pub-wiki-turing-bletchley",
    site: "wikipedia.org",
    category: "heavy",
    url: "https://en.wikipedia.org/wiki/Alan_Turing",
    prompt: "In the article body, open the link to the Bletchley Park article.",
    check: urlIs("en.wikipedia.org", "/wiki/Bletchley_Park"),
  },
  {
    id: "pub-wiki-us-jefferson",
    site: "wikipedia.org",
    category: "heavy",
    url: "https://en.wikipedia.org/wiki/United_States",
    prompt: "Find a link to the article about Thomas Jefferson in this long article and open it.",
    check: urlIs("en.wikipedia.org", "/wiki/Thomas_Jefferson"),
  },
  {
    id: "pub-github-issues",
    site: "github.com",
    category: "heavy",
    url: "https://github.com/vercel/next.js",
    prompt: "Open the Issues tab of this repository.",
    check: urlIs("github.com", "/vercel/next.js/issues"),
  },
  {
    id: "pub-mdn-htmliframe",
    site: "developer.mozilla.org",
    category: "heavy",
    url: "https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/iframe",
    prompt: "Open the page of the HTMLIFrameElement DOM interface that this element reference links to.",
    check: urlIs("developer.mozilla.org", "/en-US/docs/Web/API/HTMLIFrameElement"),
  },
  {
    id: "pub-tailwind-flex-direction",
    site: "tailwindcss.com",
    category: "heavy",
    url: "https://tailwindcss.com/docs/installation/using-vite",
    prompt: "Open the documentation page for the flex-direction utilities.",
    check: urlIs("tailwindcss.com", "/docs/flex-direction"),
  },
  {
    id: "pub-go-fmt-sprintf",
    site: "pkg.go.dev",
    category: "heavy",
    url: "https://pkg.go.dev/fmt",
    prompt: "Jump to the documentation of the function Sprintf in this package page, so that the URL ends with its anchor.",
    check: ({ url }) => u(url).pathname === "/fmt" && u(url).hash === "#Sprintf",
  },

  // ---------------- read-only extraction against runtime ground truth ----------------
  {
    id: "pub-books-price",
    site: "toscrape.com",
    category: "extract",
    url: "https://books.toscrape.com/",
    prompt: "What is the price of the book \"A Light in the Attic\" listed on this page? Answer with the price as shown.",
    async truth() {
      const html = await fetchText("https://books.toscrape.com/");
      const m = html.match(/title="A Light in the Attic"[\s\S]*?class="price_color">([^<]+)</);
      if (!m) throw new Error("truth: price not found");
      return strip(m[1]).replace(/^Â/, "");
    },
    check: ({ answer, truth }) => has(answer, String(truth).replace(/[^\d.]/g, "")),
  },
  {
    id: "pub-books-count",
    site: "toscrape.com",
    category: "extract",
    url: "https://books.toscrape.com/",
    prompt: "How many books are listed on this first page of the catalogue? Answer with the number.",
    async truth() {
      const html = await fetchText("https://books.toscrape.com/");
      return (html.match(/class="product_pod"/g) ?? []).length;
    },
    check: ({ answer, truth }) => new RegExp(`(^|[^\\d.])${truth}([^\\d]|$)`).test(answer),
  },
  {
    id: "pub-quotes-first-author",
    site: "toscrape.com",
    category: "extract",
    url: "https://quotes.toscrape.com/js/",
    prompt: "Who is the author of the first quote on this page?",
    async truth() {
      const html = await fetchText("https://quotes.toscrape.com/");
      const m = html.match(/class="author"[^>]*>([^<]+)</);
      if (!m) throw new Error("truth: author not found");
      return strip(m[1]);
    },
    check: ({ answer, truth }) => has(answer, truth),
  },
  {
    id: "pub-wiki-turing-birth",
    site: "wikipedia.org",
    category: "extract",
    url: "https://en.wikipedia.org/wiki/Alan_Turing",
    prompt: "On what date was Alan Turing born? Answer with day, month and year.",
    async truth() {
      const raw = await fetchText("https://en.wikipedia.org/w/index.php?title=Alan_Turing&action=raw");
      const m = raw.match(/birth_date\s*=\s*\{\{[^|}]*\|(?:df=y\|)?(\d+)\|(\d+)\|(\d+)/i);
      if (!m) throw new Error("truth: birth date not found");
      return { year: m[1], month: MONTHS[Number(m[2])], day: String(Number(m[3])) };
    },
    check: ({ answer, truth }) => has(answer, truth.year) && has(answer, truth.month) && new RegExp(`(^|[^\\d])${truth.day}([^\\d]|$)`).test(answer),
  },
  {
    id: "pub-hn-top-title",
    site: "news.ycombinator.com",
    category: "extract",
    url: "https://news.ycombinator.com/",
    prompt: "What is the title of the first story on the front page? Answer with the exact title.",
    async truth() {
      cache.delete("https://news.ycombinator.com/");
      const html = await fetchText("https://news.ycombinator.com/");
      const m = html.match(/class="titleline"><a[^>]*>([^<]+)</);
      if (!m) throw new Error("truth: top story not found");
      return strip(m[1]);
    },
    check: ({ answer, truth, truthAfter }) => has(answer, truth) || (truthAfter != null && has(answer, truthAfter)),
  },
  {
    id: "pub-pydocs-ensure-ascii",
    site: "docs.python.org",
    category: "extract",
    url: "https://docs.python.org/3/library/json.html",
    prompt: "What is the default value of the ensure_ascii parameter of json.dumps?",
    async truth() {
      const html = (await fetchText("https://docs.python.org/3/library/json.html")).replace(/\n/g, " ");
      const m = html.match(/json\.dumps[\s\S]{0,3000}?ensure_ascii<\/span><\/span><span class="o"><span class="pre">=<\/span><\/span><span class="default_value"><span class="pre">(\w+)</);
      if (!m) throw new Error("truth: ensure_ascii default not found");
      return m[1];
    },
    check: ({ answer, truth }) => has(answer, truth),
  },
  {
    id: "pub-github-default-branch",
    site: "github.com",
    category: "extract",
    url: "https://github.com/vercel/next.js",
    prompt: "What is the name of the default branch of this repository?",
    async truth() {
      const j = JSON.parse(await fetchText("https://api.github.com/repos/vercel/next.js"));
      if (!j.default_branch) throw new Error("truth: default branch not found");
      return j.default_branch;
    },
    check: ({ answer, truth }) => has(answer, truth),
  },
  {
    id: "pub-herokuapp-tables-email",
    site: "the-internet.herokuapp.com",
    category: "extract",
    url: "https://the-internet.herokuapp.com/tables",
    prompt: "What is the email address of Jason Doe in the first table?",
    async truth() {
      const html = await fetchText("https://the-internet.herokuapp.com/tables");
      const m = html.match(/<td>Doe<\/td>\s*<td>Jason<\/td>\s*<td>([^<]+)</);
      if (!m) throw new Error("truth: email not found");
      return strip(m[1]);
    },
    check: ({ answer, truth }) => has(answer, truth),
  },
  {
    id: "pub-httpbin-html-author",
    site: "httpbin.org",
    category: "extract",
    url: "https://httpbin.org/html",
    prompt: "Whose name appears in the heading of this page? Answer with the full name.",
    async truth() {
      const html = await fetchText("https://httpbin.org/html");
      const m = html.match(/<h1>([^<]+?) - /);
      if (!m) throw new Error("truth: heading not found");
      return strip(m[1]);
    },
    check: ({ answer, truth }) => has(answer, truth),
  },

  // ---------------- iframes ----------------
  {
    id: "pub-jqueryui-datepicker",
    site: "jqueryui.com",
    category: "iframe",
    url: "https://jqueryui.com/datepicker/",
    prompt: "In the demo, open the date picker and choose the 15th day of the month that is shown.",
    async check({ ev }) {
      const v = await ev("document.querySelector('iframe').contentDocument.querySelector('#datepicker').value");
      return /^\d\d\/15\/\d{4}$/.test(String(v));
    },
  },
  {
    id: "pub-jqueryui-checkboxradio",
    site: "jqueryui.com",
    category: "iframe",
    url: "https://jqueryui.com/checkboxradio/",
    prompt: "In the demo, select the location Paris and the hotel rating 4 Star.",
    async check({ ev }) {
      const v = await ev("(()=>{const d=document.querySelector('iframe').contentDocument;return JSON.stringify([d.querySelector('#radio-2').checked,d.querySelector('#radio-1').checked,d.querySelector('#radio-3').checked,d.querySelector('#checkbox-3').checked,d.querySelector('#checkbox-1').checked,d.querySelector('#checkbox-2').checked,d.querySelector('#checkbox-4').checked])})()");
      return v === "[true,false,false,true,false,false,false]";
    },
  },
  {
    id: "pub-jqueryui-accordion",
    site: "jqueryui.com",
    category: "iframe",
    url: "https://jqueryui.com/accordion/",
    prompt: "In the demo, open the Section 3 panel.",
    async check({ ev }) {
      const v = await ev("(()=>{const d=document.querySelector('iframe').contentDocument;const a=d.querySelector('#accordion .ui-accordion-header-active');return a?a.textContent.trim():''})()");
      return v === "Section 3";
    },
  },
  {
    id: "pub-herokuapp-nested-frames",
    site: "the-internet.herokuapp.com",
    category: "iframe",
    url: "https://the-internet.herokuapp.com/nested_frames",
    prompt: "This page is made of nested frames. What text does the middle frame of the top row show?",
    async truth() {
      const html = await fetchText("https://the-internet.herokuapp.com/frame_middle");
      const m = html.match(/<div id="content">([\s\S]*?)<\/div>/);
      if (!m) throw new Error("truth: middle frame not found");
      return strip(m[1]);
    },
    check: ({ answer, truth }) => has(answer, truth),
  },
  {
    id: "pub-demo-iframe-form",
    site: "web-form-demo",
    category: "iframe",
    url: "https://www.selenium.dev/selenium/web/iframes.html",
    prompt: "In the form inside the embedded frame, type ada@example.com in the email field and check the checkbox with id checky.",
    async check({ ev }) {
      const v = await ev("(()=>{const d=document.querySelector('iframe').contentDocument;return JSON.stringify([d.querySelector('#email').value,d.querySelector('#checky').checked])})()");
      return v === '["ada@example.com",true]';
    },
  },

  // ---------------- SPA and div-button apps ----------------
  {
    id: "pub-todomvc-react-add",
    site: "todomvc.com",
    category: "spa",
    url: "https://todomvc.com/examples/react/dist/",
    prompt: "Add three todos \"buy milk\", \"walk dog\" and \"write code\", then mark \"walk dog\" as completed.",
    async check({ ev }) {
      const v = await ev("JSON.stringify([...document.querySelectorAll('.todo-list li')].map(li=>[li.innerText.trim(),li.classList.contains('completed')]))");
      return v === '[["buy milk",false],["walk dog",true],["write code",false]]';
    },
  },
  {
    id: "pub-todomvc-vue-filter",
    site: "todomvc.com",
    category: "spa",
    url: "https://todomvc.com/examples/vue/dist/",
    prompt: "Add the todos \"a\", \"b\" and \"c\", complete \"a\" and \"b\", then show only the active todos.",
    async check({ ev }) {
      const v = await ev("JSON.stringify([location.hash,[...document.querySelectorAll('.todo-list li')].map(li=>li.innerText.trim())])");
      return v === '["#/active",["c"]]';
    },
  },
  {
    id: "pub-todomvc-react-clear",
    site: "todomvc.com",
    category: "spa",
    url: "https://todomvc.com/examples/react/dist/",
    prompt: "Add the todos \"one\", \"two\" and \"three\", complete \"one\" and \"two\", and clear the completed todos so only \"three\" remains.",
    async check({ ev }) {
      const v = await ev("JSON.stringify([...document.querySelectorAll('.todo-list li')].map(li=>li.innerText.trim()))");
      return v === '["three"]';
    },
  },
  {
    id: "pub-todomvc-vue-edit",
    site: "todomvc.com",
    category: "spa",
    url: "https://todomvc.com/examples/vue/dist/",
    prompt: "Add the todo \"draft\", then edit it so that it reads \"final\".",
    async check({ ev }) {
      const v = await ev("JSON.stringify([...document.querySelectorAll('.todo-list li')].map(li=>li.innerText.trim()))");
      return v === '["final"]';
    },
  },
];
