// Harness self-check for the public tasks: every check must fail on the untouched page and pass
// after a scripted solution (direct DOM or URL manipulation). No model is involved.
//
//   bun run snapshot-source/validate-public.ts [task-id ...]

import { publicTasks } from "./public-tasks.ts";
import { cliEnv } from "./guard";

const bin = process.env.AGENT_BROWSER_BIN ?? `${import.meta.dir}/../../cli/target/ci/agent-browser`;
const session = "abv3-validate";
async function ab(...args: string[]) {
  const p = Bun.spawn([bin, ...args], { stdout: "pipe", stderr: "pipe", env: cliEnv({ AGENT_BROWSER_SESSION: session }) });
  const [o, e] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text()]);
  await p.exited;
  return (o + e).trim();
}
const ev = async (js: string) => {
  const r = await ab("eval", js);
  try {
    return JSON.parse(r.split("\n")[0]);
  } catch {
    return r;
  }
};
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const fillIn = (pairs: Record<string, string>) =>
  Object.entries(pairs).map(([sel, v]) => `(()=>{const e=document.querySelector(${JSON.stringify(sel)});e.value=${JSON.stringify(v)};e.dispatchEvent(new Event('input',{bubbles:true}));e.dispatchEvent(new Event('change',{bubbles:true}))})()`).join(";");

// Solutions: a URL to open, JS to run, or an answer built from the ground truth.
const oracle: Record<string, { nav?: string; js?: string; answer?: (t: any) => string; wait?: number }> = {
  "pub-httpbin-pizza": { js: `${fillIn({ "[name=custname]": "Ada Lovelace", "[name=custtel]": "555-0100", "[name=custemail]": "ada@example.com", "[name=delivery]": "18:30", "[name=comments]": "Ring twice" })};document.querySelector('[name=size][value=large]').checked=true;document.querySelector('[name=topping][value=bacon]').checked=true;document.querySelector('[name=topping][value=onion]').checked=true;document.forms[0].submit()` },
  "pub-httpbin-pizza-small": { js: `${fillIn({ "[name=custname]": "Grace Hopper", "[name=custtel]": "555-0199", "[name=custemail]": "grace@example.com" })};document.querySelector('[name=size][value=small]').checked=true;document.querySelector('[name=topping][value=mushroom]').checked=true;document.forms[0].submit()` },
  "pub-demo-webform": { js: `${fillIn({ "[name=my-text]": "hello world", "[name=my-textarea]": "second line", "[name=my-select]": "2" })};document.forms[0].submit()` },
  "pub-demo-webform-checks": { js: `${fillIn({ "[name=my-text]": "abc" })};document.querySelectorAll('[name=my-check]').forEach(c=>c.checked=true);document.forms[0].submit()` },
  "pub-herokuapp-dropdown": { js: `${fillIn({ "#dropdown": "2" })}` },
  "pub-herokuapp-checkboxes": { js: `(()=>{const c=document.querySelectorAll('#checkboxes input');c[0].checked=true;c[1].checked=false})()` },
  "pub-herokuapp-add-remove": { js: `(()=>{const b=document.querySelector('button[onclick=addElement\\\\(\\\\)]');for(let i=0;i<4;i++)b.click();document.querySelector('.added-manually').click()})()` },
  "pub-wiki-search-ada": { nav: "https://en.wikipedia.org/wiki/Ada_Lovelace" },
  "pub-pydocs-loads": { nav: "https://docs.python.org/3/library/json.html#json.loads" },
  "pub-reactdev-props": { nav: "https://react.dev/learn/passing-props-to-a-component" },
  "pub-rustbook-ownership": { nav: "https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html" },
  "pub-vue-reactivity": { nav: "https://vuejs.org/guide/essentials/reactivity-fundamentals.html" },
  "pub-example-learn-more": { nav: "https://www.iana.org/help/example-domains" },
  "pub-hn-newest": { nav: "https://news.ycombinator.com/newest" },
  "pub-books-travel": { nav: "https://books.toscrape.com/catalogue/category/books/travel_2/index.html" },
  "pub-quotes-js-page3": { nav: "https://quotes.toscrape.com/js/page/3/" },
  "pub-wiki-turing-bletchley": { nav: "https://en.wikipedia.org/wiki/Bletchley_Park" },
  "pub-wiki-us-jefferson": { nav: "https://en.wikipedia.org/wiki/Thomas_Jefferson" },
  "pub-github-issues": { nav: "https://github.com/vercel/next.js/issues" },
  "pub-mdn-htmliframe": { nav: "https://developer.mozilla.org/en-US/docs/Web/API/HTMLIFrameElement" },
  "pub-tailwind-flex-direction": { nav: "https://tailwindcss.com/docs/flex-direction" },
  "pub-go-fmt-sprintf": { nav: "https://pkg.go.dev/fmt#Sprintf" },
  "pub-books-price": { answer: (t) => `The price is ${t}` },
  "pub-books-count": { answer: (t) => `${t} books` },
  "pub-quotes-first-author": { answer: (t) => t },
  "pub-wiki-turing-birth": { answer: (t) => `${t.day} ${t.month} ${t.year}` },
  "pub-hn-top-title": { answer: (t) => t },
  "pub-pydocs-ensure-ascii": { answer: (t) => `It defaults to ${t}` },
  "pub-github-default-branch": { answer: (t) => t },
  "pub-herokuapp-tables-email": { answer: (t) => t },
  "pub-httpbin-html-author": { answer: (t) => t },
  "pub-herokuapp-nested-frames": { answer: (t) => t },
  "pub-jqueryui-datepicker": { js: `(()=>{const d=document.querySelector('iframe').contentDocument;const i=d.querySelector('#datepicker');i.focus();i.click();setTimeout(()=>{[...d.querySelectorAll('.ui-datepicker-calendar a')].find(a=>a.textContent==='15').click()},500)})()`, wait: 1500 },
  "pub-jqueryui-checkboxradio": { js: `(()=>{const d=document.querySelector('iframe').contentDocument;d.querySelector('label[for=radio-2]').click();d.querySelector('label[for=checkbox-3]').click()})()` },
  "pub-jqueryui-accordion": { js: `(()=>{const d=document.querySelector('iframe').contentDocument;d.querySelectorAll('#accordion h3')[2].click()})()`, wait: 800 },
  "pub-demo-iframe-form": { js: `(()=>{const d=document.querySelector('iframe').contentDocument;const e=d.querySelector('#email');e.value='ada@example.com';d.querySelector('#checky').checked=true})()` },
  "pub-todomvc-react-add": { js: "ADD:buy milk,walk dog,write code|DONE:walk dog" },
  "pub-todomvc-vue-filter": { js: "ADD:a,b,c|DONE:a,b|HASH:#/active" },
  "pub-todomvc-react-clear": { js: "ADD:one,two,three|DONE:one,two|CLEAR" },
  "pub-todomvc-vue-edit": { js: "ADD:draft|EDIT:draft>final" },
};

async function todoScript(spec: string) {
  for (const part of spec.split("|")) {
    const [op, arg = ""] = part.split(/:(.*)/s);
    if (op === "ADD") {
      for (const t of arg.split(",")) {
        await ev(`(()=>{const i=document.querySelector('.new-todo');const set=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;set.call(i,${JSON.stringify(t)});i.dispatchEvent(new Event('input',{bubbles:true}));i.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',code:'Enter',keyCode:13,bubbles:true}));i.dispatchEvent(new KeyboardEvent('keyup',{key:'Enter',code:'Enter',keyCode:13,bubbles:true}))})()`);
        await sleep(200);
      }
    } else if (op === "DONE") {
      for (const t of arg.split(",")) await ev(`[...document.querySelectorAll('.todo-list li')].find(li=>li.innerText.trim()==${JSON.stringify(t)}).querySelector('.toggle').click()`);
    } else if (op === "HASH") await ev(`location.hash=${JSON.stringify(arg)}`);
    else if (op === "CLEAR") await ev(`document.querySelector('.clear-completed').click()`);
    else if (op === "EDIT") {
      const [from, to] = arg.split(">");
      await ev(`(()=>{const li=[...document.querySelectorAll('.todo-list li')].find(l=>l.innerText.trim()==${JSON.stringify(from)});li.querySelector('label').dispatchEvent(new MouseEvent('dblclick',{bubbles:true}))})()`);
      await sleep(200);
      await ev(`(()=>{const i=document.querySelector('.todo-list li .edit');const set=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;set.call(i,${JSON.stringify(to)});i.dispatchEvent(new Event('input',{bubbles:true}));i.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',code:'Enter',keyCode:13,bubbles:true}));i.dispatchEvent(new KeyboardEvent('keyup',{key:'Enter',code:'Enter',keyCode:13,bubbles:true}))})()`);
    }
    await sleep(200);
  }
}

const only = process.argv.slice(2);
let bad = 0;
for (const t of publicTasks) {
  if (only.length && !only.includes(t.id)) continue;
  const o = oracle[t.id];
  if (!o) {
    console.log(`NOORACLE ${t.id}`);
    bad++;
    continue;
  }
  let truth: any;
  try {
    truth = t.truth ? await t.truth() : undefined;
  } catch (e) {
    console.log(`TRUTHFAIL ${t.id} ${e}`);
    bad++;
    continue;
  }
  await ab("open", t.url);
  await sleep(500);
  const ctx = async (answer: string) => ({ url: (await ab("get", "url")).split("\n")[0], answer, truth, truthAfter: truth, ev });
  const neg = await t.check(await ctx(""));
  if (o.nav) await ab("open", o.nav);
  if (o.js?.startsWith("ADD:")) await todoScript(o.js);
  else if (o.js) {
    await ab("eval", o.js);
    await sleep(o.wait ?? 1500);
  }
  const pos = await t.check(await ctx(o.answer ? o.answer(truth) : ""));
  const ok = !neg && pos;
  if (!ok) bad++;
  console.log(`${ok ? "OK  " : "BAD "} ${t.id} negative=${neg} positive=${pos} truth=${JSON.stringify(truth)}`);
  await sleep(1500);
}
await ab("close");
console.log(bad ? `${bad} problems` : "all ok");
