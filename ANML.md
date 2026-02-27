# ANML — Agent Native Markup Language

**Version:** 0.1.0-draft
**Status:** RFC / Proposal
**Authors:** Built on patterns from [agent-browser](./README.md)

---

## Abstract

HTML was designed for humans — rich, visual, redundant, forgiving. ANML is its
counterpart for agents: minimal, structured, deterministic, and
context-window-aware. Where HTML optimizes for rendering in a browser viewport,
ANML optimizes for reasoning inside an LLM context window.

ANML formalizes the patterns that emerged organically in `agent-browser`'s ARIA
snapshot format and generalizes them into a markup language suitable for any
agent-facing infrastructure — browsers, terminals, databases, cloud consoles,
IDEs, and beyond.

---

## Design Principles

1. **Every token earns its place.** Context windows are finite. No closing tags,
   no redundant attributes, no syntactic ceremony.

2. **Refs are first-class citizens.** Elements that can be acted upon have
   stable, compact identifiers. Refs are not metadata bolted on — they are
   identity.

3. **Structure through indentation.** Hierarchy is expressed by whitespace, not
   by paired delimiters. Two spaces per level. No ambiguity.

4. **Semantic over visual.** Roles describe what something *is* (`button`,
   `textbox`, `heading`), never what it *looks like* (`div`, `span`, `bold`).

5. **Interactive elements are loud.** Agents need to instantly find what they can
   act on. Interactive elements carry refs and are visually distinct from passive
   content.

6. **Built-in budget controls.** Depth limiting, role filtering, and content
   summarization are part of the format — not afterthoughts.

7. **Diffable by design.** Incremental updates send only what changed, using a
   native diff syntax.

8. **Line-oriented and streamable.** Every line is independently parseable. No
   buffering the whole document to start processing.

---

## 1. Document Structure

An ANML document consists of an optional **frontmatter** block followed by a
**body** of indented element lines.

```anml
---
url: https://example.com
title: Example Domain
viewport: 1280x720
ts: 1709071200
---
nav
  link#e1 "Home" href=/
  link#e2 "About" href=/about
main
  h1#e3 "Welcome to Example" level=1
  p "Paragraph of introductory text."
  form#e4
    textbox#e5 "Email" placeholder="you@example.com"
    textbox#e6 "Password" [masked]
    button#e7 "Sign In" [primary]
```

### 1.1 Frontmatter

Delimited by `---`. Contains `key: value` metadata about the document source.
All fields are optional. Reserved keys:

| Key        | Description                          | Example                        |
|------------|--------------------------------------|--------------------------------|
| `url`      | Source URL or resource identifier     | `https://example.com`          |
| `title`    | Human-readable title                 | `Example Domain`               |
| `viewport` | Viewport dimensions                  | `1280x720`                     |
| `ts`       | Unix timestamp of snapshot           | `1709071200`                   |
| `source`   | Infrastructure type                  | `browser`, `terminal`, `db`    |
| `budget`   | Context budget hint (max lines)      | `200`                          |
| `encoding` | Content encoding                     | `utf-8`                        |
| `depth`    | Max depth captured                   | `5`                            |
| `filter`   | Active filters                       | `interactive`, `compact`       |

### 1.2 Body

The body is a tree of elements expressed through indentation (2 spaces per
level). Each line describes one element.

---

## 2. Element Syntax

```
[indent]role[#ref] ["name"] [attributes...] [states...]
```

### 2.1 Roles

The element type. Lowercase, no quotes. Derived from ARIA roles but simplified
for cross-domain use.

**Interactive roles** (always get refs):
```
button  link  textbox  checkbox  radio  combobox  listbox
menuitem  option  searchbox  slider  spinbutton  switch  tab
treeitem  select  toggle  command
```

**Content roles** (get refs when named):
```
h1  h2  h3  h4  h5  h6  cell  item  article  region  section
img  video  audio  code  quote  label  caption  badge  chip
```

**Structural roles** (no refs, provide hierarchy):
```
nav  main  header  footer  aside  form  table  row  list  grid
group  toolbar  tablist  tree  menu  dialog  panel  card  divider
```

**Domain-specific roles** (extensible per infrastructure):
```
# Terminal
prompt  output  cursor  statusbar

# Database
resultset  column  record  schema

# Cloud
resource  service  endpoint  policy  metric

# IDE
file  function  class  variable  diagnostic  diff
```

### 2.2 Refs

Refs are compact, deterministic identifiers for actionable elements. Attached
with `#` — like CSS IDs but ephemeral (valid only within this snapshot).

```anml
button#e1 "Submit"          -- agent can act: click #e1
link#e2 "Documentation"     -- agent can act: click #e2
textbox#e3 "Search"         -- agent can act: fill #e3 "query"
h1#e4 "Page Title"          -- agent can act: text #e4
nav                          -- no ref: structural only
```

Ref format: `e` followed by a monotonically increasing integer. Refs reset per
snapshot. When duplicate role+name pairs exist, agents use the ref to
disambiguate — no `[nth=N]` annotation needed because refs are already unique.

### 2.3 Names

The accessible name or text content. Always double-quoted. Optional.

```anml
button#e1 "Submit"           -- name is "Submit"
textbox#e2 "Email address"   -- name/label is "Email address"
p "Some paragraph text."     -- text content
img "Company logo"           -- alt text
nav                          -- unnamed structural element
```

For long text content, use the `>` continuation marker:

```anml
article#e1 "Introduction"
  > This is a longer block of text content that would waste
  > tokens if the agent doesn't need it. Content under > markers
  > can be excluded by budget-aware renderers.
```

### 2.4 Attributes

`key=value` pairs. No quotes needed for simple values. Quotes for values with
spaces.

```anml
textbox#e1 "Email" placeholder="you@example.com" maxlength=100
link#e2 "Docs" href=/docs target=_blank
img#e3 "Logo" src=/logo.png width=200 height=50
slider#e4 "Volume" min=0 max=100 value=75 step=1
h1#e5 "Title" level=1
table rows=15 cols=4
```

### 2.5 States

Boolean states in `[brackets]`. Represent current element state.

```anml
checkbox#e1 "Remember me" [checked]
button#e2 "Submit" [disabled]
treeitem#e3 "src/" [expanded]
tab#e4 "Settings" [selected]
dialog#e5 "Confirm" [modal] [open]
textbox#e6 "Password" [masked] [required]
menuitem#e7 "Bold" [pressed]
region "Sidebar" [hidden]
```

---

## 3. Tables

Tables use pipe syntax for compact, readable data:

```anml
table#e1 "Users" rows=3 cols=3
  | Name    | Age | Role   |
  | Alice   | 30  | Admin  |
  | Bob     | 25  | Editor |
  | Charlie | 35  | Viewer |
```

For interactive table cells, refs attach to individual cells:

```anml
table "Permissions"
  | User  | Read          | Write          |
  | Alice | checkbox#e1 [checked] | checkbox#e2 [checked]  |
  | Bob   | checkbox#e3 [checked] | checkbox#e4            |
```

Large tables use the `~` summarization marker:

```anml
table "Logs" rows=10482 cols=5
  | Timestamp  | Level | Message              |
  | 2024-02-27 | ERROR | Connection refused   |
  | 2024-02-27 | ERROR | Timeout after 30s    |
  ~ ... 10478 rows omitted (2 ERROR, 847 WARN, 9633 INFO)
  | 2024-02-01 | INFO  | Service started      |
```

---

## 4. Budget Controls

ANML is designed for context windows, not infinite scrollback. Budget controls
are part of the format.

### 4.1 Depth Limiting

Frontmatter `depth: N` or renderer option. Elements beyond the depth are
replaced with a summary:

```anml
main
  nav
    ~ 12 links
  section#e1 "Content"
    ~ 3 headings, 2 forms, 45 paragraphs
  aside
    ~ 8 items
```

### 4.2 Content Folding

The `~` prefix marks summarized content. Agents know this is a lossy
representation and can request expansion:

```anml
article#e1 "Terms of Service"
  ~ 2847 words, 24 sections (use 'expand #e1' to load)
```

### 4.3 Role Filtering

The `filter` frontmatter key records active filters:

```anml
---
filter: interactive
---
button#e1 "Submit"
textbox#e2 "Email"
link#e3 "Cancel"
```

All non-interactive content has been stripped. The agent knows this and can
request a full snapshot if needed.

---

## 5. Diffs

When the same page/resource is snapshotted repeatedly, ANML supports incremental
diffs to avoid re-sending the full tree. Diffs use `+`, `-`, and `*` prefixes.

```anml
---
type: diff
base: snapshot-42
ts: 1709071260
---
* textbox#e3 "Search" value="hello wor"
  + item#e12 "hello world" [highlighted]
  + item#e13 "hello worlds apart"
- dialog#e8 "Cookie consent"
+ banner#e14 "Welcome back, Alice"
```

| Prefix | Meaning                        |
|--------|--------------------------------|
| `+`    | Element added                  |
| `-`    | Element removed                |
| `*`    | Element modified (attrs/state) |
| (none) | Context line (unchanged)       |

Diffs preserve refs from the base snapshot where possible. New elements get new
refs. Removed element refs are invalidated.

---

## 6. Actions (Companion Protocol)

ANML describes state. The **action protocol** describes mutations. Actions
reference elements by ref:

```
click #e1                          -- click a button/link
fill #e3 "hello@example.com"       -- type into a textbox
check #e5                          -- check a checkbox
uncheck #e5                        -- uncheck a checkbox
select #e6 "Option B"              -- select from combobox
scroll #e1 down                    -- scroll within element
hover #e2                          -- hover over element
expand #e4                         -- expand collapsed content
keys #e3 "Enter"                   -- send keystrokes
```

Actions return a new ANML snapshot (or diff) reflecting the updated state.

---

## 7. Cross-Domain Examples

ANML is not browser-specific. Here's how it adapts to other infrastructure:

### 7.1 Terminal Session

```anml
---
source: terminal
shell: bash
cwd: /home/user/project
ts: 1709071200
---
prompt "$ " value="git status"
output
  > On branch main
  > Changes not staged for commit:
  >   modified: src/app.ts
  >   modified: src/utils.ts
  > Untracked files:
  >   src/new-feature.ts
statusbar "main | 2M 1U | node v20.11"
```

### 7.2 Database Query Result

```anml
---
source: db
engine: postgresql
database: myapp_prod
query: "SELECT * FROM users WHERE active = true"
duration_ms: 12
ts: 1709071200
---
resultset rows=847 cols=4
  | id  | name    | email              | created_at |
  | 1   | Alice   | alice@example.com  | 2024-01-15 |
  | 2   | Bob     | bob@example.com    | 2024-01-16 |
  | 3   | Charlie | charlie@ex.com     | 2024-01-17 |
  ~ ... 844 rows omitted
schema
  column "id" type=serial [primary_key]
  column "name" type=varchar(255) [not_null]
  column "email" type=varchar(255) [unique] [not_null]
  column "created_at" type=timestamp [not_null] default=now()
```

### 7.3 Cloud Infrastructure

```anml
---
source: cloud
provider: aws
region: us-east-1
account: 123456789012
ts: 1709071200
---
service "ECS"
  resource#r1 "api-cluster" type=cluster status=ACTIVE
    resource#r2 "api-service" type=service desired=3 running=3 pending=0
      resource#r3 "api-task-1" type=task status=RUNNING cpu=25% mem=512MB
      resource#r4 "api-task-2" type=task status=RUNNING cpu=30% mem=480MB
      resource#r5 "api-task-3" type=task status=RUNNING cpu=22% mem=501MB
service "RDS"
  resource#r6 "main-db" type=postgres engine=15.4 status=available
    metric "CPU" value=12% avg_1h=15%
    metric "Connections" value=45 max=100
    metric "Storage" value=24GB total=100GB
service "Lambda"
  resource#r7 "image-resize" type=function runtime=python3.12
    metric "Invocations" value=1247 period=1h errors=3
  resource#r8 "email-sender" type=function runtime=nodejs20
    metric "Invocations" value=89 period=1h errors=0
```

### 7.4 IDE / Code Navigation

```anml
---
source: ide
project: agent-browser
language: typescript
file: src/snapshot.ts
ts: 1709071200
---
file "src/snapshot.ts" lines=650 modified=true
  function#f1 "resetRefs" line=56 exported
  function#f2 "nextRef" line=63
  function#f3 "getIndentLevel" line=130
  function#f4 "processAriaTree" line=390 exported
    variable "lines" type=string[] line=391
    variable "result" type=string[] line=392
  function#f5 "compactTree" line=561
  function#f6 "parseRef" line=605 exported
  function#f7 "getSnapshotStats" line=618 exported
diagnostic "Unused variable 'temp'" line=245 severity=warning
diagnostic "Type error: string not assignable to number" line=312 severity=error
```

---

## 8. Formal Grammar (EBNF)

```ebnf
document     = [ frontmatter ] body ;
frontmatter  = "---" newline { meta_line } "---" newline ;
meta_line    = key ":" value newline ;

body         = { element_line | content_line | table_line
               | summary_line | diff_line | blank_line } ;

element_line = indent role [ "#" ref ] [ name ] { attribute } { state } newline ;
content_line = indent ">" text newline ;
table_line   = indent "|" { cell "|" } newline ;
summary_line = indent "~" text newline ;
diff_line    = ("+" | "-" | "*") element_line ;
blank_line   = newline ;

indent       = { "  " } ;                   (* 2 spaces per level *)
role         = letter { letter | digit } ;
ref          = "e" digit { digit }           (* browser *)
             | "r" digit { digit }           (* cloud resource *)
             | "f" digit { digit }           (* IDE function *)
             | letter digit { digit } ;      (* generic *)
name         = '"' { char } '"' ;
attribute    = key "=" value ;
state        = "[" identifier "]" ;
key          = identifier ;
value        = identifier | quoted_string ;
```

---

## 9. Comparison with Existing Formats

| Property               | HTML     | JSON     | Markdown | ARIA Snapshot | **ANML**     |
|------------------------|----------|----------|----------|---------------|--------------|
| Token efficiency       | Poor     | Medium   | Good     | Good          | **Best**     |
| Hierarchy              | Tags     | Nesting  | Headers  | Indentation   | **Indentation** |
| Machine parseable      | Complex  | Yes      | Fragile  | Ad-hoc        | **Yes**      |
| Refs / actionability   | No       | No       | No       | Bolted on     | **Native**   |
| Budget controls        | No       | No       | No       | Options       | **Native**   |
| Diff support           | No       | Patch    | No       | No            | **Native**   |
| Cross-domain           | Browser  | Generic  | Docs     | Browser       | **Universal**|
| Streaming              | Partial  | No       | Yes      | Yes           | **Yes**      |
| Human readable         | Medium   | Poor     | Best     | Good          | **Good**     |
| Standardized           | Yes (W3C)| Yes (RFC)| Yes (CM) | No            | **Proposed** |

---

## 10. Token Budget Analysis

The same login form represented in each format:

**HTML (278 tokens):**
```html
<nav><a href="/">Home</a><a href="/about">About</a></nav>
<main><h1>Welcome</h1><form><label for="email">Email</label>
<input type="email" id="email" placeholder="you@example.com">
<label for="pw">Password</label><input type="password" id="pw">
<button type="submit">Sign In</button></form></main>
```

**JSON (195 tokens):**
```json
{"nav":{"links":[{"text":"Home","href":"/"},{"text":"About","href":"/about"}]},
"main":{"h1":"Welcome","form":{"fields":[{"type":"email","label":"Email",
"placeholder":"you@example.com"},{"type":"password","label":"Password"}],
"submit":{"text":"Sign In"}}}}
```

**ARIA snapshot, current format (112 tokens):**
```
- navigation:
  - link "Home" [ref=e1]
  - link "About" [ref=e2]
- main:
  - heading "Welcome" [ref=e3] [level=1]
  - form:
    - textbox "Email" [ref=e4]
    - textbox "Password" [ref=e5]
    - button "Sign In" [ref=e6]
```

**ANML (82 tokens):**
```anml
nav
  link#e1 "Home" href=/
  link#e2 "About" href=/about
main
  h1#e3 "Welcome"
  form
    textbox#e4 "Email" placeholder="you@example.com"
    textbox#e5 "Password" [masked]
    button#e6 "Sign In"
```

**ANML interactive-only (38 tokens):**
```anml
link#e1 "Home"
link#e2 "About"
textbox#e4 "Email"
textbox#e5 "Password"
button#e6 "Sign In"
```

ANML achieves **~70% token reduction** vs HTML and **~27% reduction** vs the
current ARIA snapshot format — while adding features (attributes, states,
budget controls) the ARIA format lacks.

---

## 11. Migration Path from Current Format

The current `agent-browser` ARIA snapshot format is a subset of ANML. Migration
is incremental:

| Step | Change                         | Breaking? |
|------|--------------------------------|-----------|
| 1    | Move `[ref=e1]` to `#e1`      | Yes*      |
| 2    | Drop `- ` prefix              | Yes*      |
| 3    | Drop trailing `:`             | No        |
| 4    | Add frontmatter               | No        |
| 5    | Replace `[level=N]` with `hN` | No        |
| 6    | Add `[state]` syntax          | No        |
| 7    | Add `~` summarization         | No        |
| 8    | Add `>` content blocks        | No        |
| 9    | Add diff support              | No        |

\* A compatibility shim can support both formats during transition.

---

## 12. Design Rationale: Why Not Just Use X?

**Why not JSON?** Too verbose for hierarchical content. Braces and quotes
consume tokens. Not human-scannable. No native hierarchy visualization.

**Why not YAML?** Close in spirit, but YAML's complexity (anchors, flow vs block,
implicit typing) creates parsing ambiguity. ANML takes YAML's indentation and
nothing else.

**Why not Markdown?** Not machine-parseable reliably. No refs. No semantic roles.
No action protocol. Great for documents, wrong for interactive interfaces.

**Why not HTML?** Closing tags double the overhead. Visual semantics (`<div>`,
`<span>`) are meaningless to agents. Attributes are verbose
(`class="btn btn-primary"`). The entire design assumes a visual renderer.

**Why not the current ARIA snapshot?** It works — ANML evolves it. The current
format is ad-hoc with no spec, no cross-domain story, no diff support, no
budget controls, and wastes tokens on `- ` prefixes and `[ref=eN]` brackets.

---

## 13. Open Questions

- **Binary content:** How should ANML handle images, PDFs, audio? Current
  thinking: refs to external content with metadata (`img#e1 "Chart" src=@blob1
  width=800 height=400 alt="Revenue trend showing 20% growth"`).

- **Bidirectional refs:** Should elements be able to reference each other?
  (`label#e1 "Email" for=#e2` + `textbox#e2 "Email"`).

- **Namespacing:** For multi-source documents (browser + terminal), should there
  be namespace prefixes? (`browser:button#e1` vs `term:command#t1`).

- **Schema validation:** Should ANML documents declare their role vocabulary?
  Or is the open set of roles a feature?

- **Compression:** For very large snapshots, is a binary ANML encoding worth
  specifying? (Probably not — the target consumer is a text-based LLM.)

---

## License

This specification is released under the MIT License.
