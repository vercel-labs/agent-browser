# Agent-first browser tools: survey and comparison

Date: 2026-11-08. Scope: how other agent-first browser tools target elements, run their control loops, navigate, guard actions, and perform, compared with the proposal to use TypeSafe's Jev as a per-step resolver inside agent-browser's deterministic loop, with the caller owning the goal. No source code was changed. Related: [jev-support.md](jev-support.md).

## Summary

- The closest existing match to the proposal is Notte's `act()`: Jev (or an LLM) picks one offered option, invalid answers execute nothing, a fresh-page check runs before acting, and the caller owns the loop ([Notte act docs](https://docs.notte.cc/features/sessions/act)). browser-use's `jev-ultrafast` uses Jev the same way but owns the goal loop itself ([repo](https://github.com/browser-use/jev-ultrafast)).
- The only per-call Jev latency from a real run is a 178 ms median over 17 requests in one flight search, from a single task on one profile ([performance report](https://raw.githubusercontent.com/browser-use/jev-ultrafast/main/docs/performance.md)). TypeSafe's own figures disagree with each other (about 100 ms in docs, 70 to 500 ms in its launch post).
- No public source measures Jev's accuracy on web candidate lists. The accuracy gains in the field came from planner and validator loops, not from a judgment-only resolver. Skyvern went from about 45% to 85.85% on WebVoyager by adding a planner and then a validator (vendor-reported, [Skyvern 2.0](https://www.skyvern.com/blog/skyvern-2-0-state-of-the-art-web-navigation-with-85-8-on-webvoyager-eval/)).
- Zero-model paths (deterministic refs, replayed actions, cache hits) are the fastest per action. Among model-in-loop patterns, a single Choice is the only one with a published per-call number.
- Bounded loops with a step cap and repeat detection are standard. Hidden agent loops are being unbundled: Notte removed its built-in agent loop in 1.10 ([Notte README](https://raw.githubusercontent.com/nottelabs/notte/main/README.md)).
- Redirects are the known hole in origin allowlists. Playwright MCP states its origin lists "does not affect redirects" ([Playwright MCP README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)). Our `--allowed-domains` already covers more request types than that (see agent-browser below), but the redirect path needs a test.
- Recommended adoptions: a strict offered-option contract with dry run, a confidence gate and a target re-check; operation plus target decisions after the click-only slice; actionable visible candidates only; a caller loop with a cap and repeat guard; confirmation returns for destructive labels. Rejected: hidden planners in the CLI, vision-first defaults, origin-only allowlists, multi-step instruction strings, and vendor latency or accuracy claims as acceptance criteria.

## Method and caveats

- Primary sources only: vendor documentation, GitHub repositories, arXiv papers and vendor blogs. Where I read a search excerpt rather than the full page, the citation is marked "(excerpt)".
- Vendor numbers are reported as claimed. Anything I could not verify is marked "unverified". Gaps are listed at the end.
- "Vercel agent-browser" is this repository, not a separate tool. The package manifest names `https://github.com/vercel-labs/agent-browser.git` as its repository ([package.json](../../package.json), version 0.41.2).

## Tools

### browser-use

Sources: [README](https://raw.githubusercontent.com/browser-use/browser-use/main/README.md), [agent parameters](https://docs.browser-use.com/open-source/customize/agent/all-parameters.md), [agent basics](https://docs.browser-use.com/open-source/customize/agent/basics.md).

- Targeting: DOM plus screenshots. The serializer marks native controls, ARIA roles, JavaScript click listeners and pointer cursors as interactive and assigns each an index ([clickable_elements.py](https://github.com/browser-use/browser-use/blob/d19ec6ef/browser_use/dom/serializer/clickable_elements.py)). Vision is on by default (`use_vision`, default True, [parameters](https://docs.browser-use.com/open-source/customize/agent/all-parameters.md)). A third-party guide says the selector map is rebuilt when navigation, focus or scroll changes ([guide](https://github.com/sandgardenhq/dh-library/blob/main/content/en/browser-use/03-how-the-agent-sees-the-page.md)). I did not verify this in code.
- Control loop: one agent. Each step is one model call that can return up to `max_actions_per_step` actions (default 5), executed until the page changes. The run cap `max_steps` defaults to 100, `max_failures` to 5, and `step_timeout` to 180 s ([basics](https://docs.browser-use.com/open-source/customize/agent/basics.md), [parameters](https://docs.browser-use.com/open-source/customize/agent/all-parameters.md)). That is about one model call per step, not per action.
- Speed: Cloud docs report median successful production task times of about 1, 2 and 5 minutes for V2, V3 and V4 (July 2026, Claude Opus 4.7). On hard internal tasks, V3 took about 6 minutes and V2 and V4 about 10 ([choosing an agent](https://docs.browser-use.com/cloud/choosing-an-agent.md)). No per-step latency is published.
- Navigation: `allowed_domains` is a Browser option ([authentication](https://docs.browser-use.com/open-source/customize/browser/authentication.md)). Multi-hop goals run inside the agent loop. Tab switching exists (timeouts are listed in [parameters](https://docs.browser-use.com/open-source/customize/agent/all-parameters.md)). I found no documented pagination or tab-by-intent method.
- Safety: `sensitive_data` placeholders keep secret values out of the LLM context, with domain-scoped credentials, and `use_vision=False` is recommended for sensitive pages ([authentication](https://docs.browser-use.com/open-source/customize/browser/authentication.md)). I found no destructive-action confirmation or injection defense in the pages read. The docs separate `is_done` (a terminal action) from `is_successful` (the agent's own report) and say to verify important external actions independently ([Cloud docs](https://docs.browser-use.com/cloud/choosing-an-agent.md)).
- Accuracy: Browser Use's internal "Bench Hard" (106 tasks, July 2026, Claude Opus 4.8, one judge): V4 76%, V3 67%, V2 54% ([choosing an agent](https://docs.browser-use.com/cloud/choosing-an-agent.md)). Internal and not reproducible from the page. BU Bench V2.1 (200 tasks) scores are in images, not text ([benchmark README](https://raw.githubusercontent.com/browser-use/benchmark/main/README.md)).
- Design notes. Copy: replaying a saved run with no model call per step, which raises a `RuntimeError` at the first step it cannot match ([README FAQ](https://raw.githubusercontent.com/browser-use/browser-use/main/README.md)), and the split between "done" and "successful". Avoid: vision on by default, and an agent that owns the goal with only a run-level cap.

### Stagehand (Browserbase)

Sources: [README](https://raw.githubusercontent.com/browserbase/stagehand/main/README.md), [act](https://docs.stagehand.dev/v4/basics/act), [observe](https://docs.stagehand.dev/v4/basics/observe), [caching](https://docs.stagehand.dev/v4/best-practices/caching).

- Targeting: hybrid. Snapshots merge every frame's accessibility tree, and the README calls the approach "hybrid accessibility-tree trimming" ([act](https://docs.stagehand.dev/v4/basics/act), [README](https://raw.githubusercontent.com/browserbase/stagehand/main/README.md)). `observe()` returns Action objects with `selector`, `method` and `arguments`. Replaying an Action uses the selector with no inference. `selfHeal` re-infers once when a selector breaks ([observe](https://docs.stagehand.dev/v4/basics/observe), [act](https://docs.stagehand.dev/v4/basics/act)).
- Control loop: deterministic code with LLM calls at decision points. `act(instruction)` runs one inference to find the target. `act(Action)` runs none. `observe` runs one inference and returns candidates ([act](https://docs.stagehand.dev/v4/basics/act), [observe](https://docs.stagehand.dev/v4/basics/observe)). The caller writes the loop. The only documented bound is a per-call timeout in milliseconds. I found no step cap.
- Speed: the README claims "2x faster execution than Playwright cloud equivalent browsers" on Browserbase. The method is not published. Replays and cache hits make zero LLM calls ([caching](https://docs.stagehand.dev/v4/best-practices/caching)). No per-step latency.
- Navigation: "Multi-step instructions are unreliable. Sequence them yourself instead" ([act](https://docs.stagehand.dev/v4/basics/act)). Tabs are handled by passing a `page` option ([act](https://docs.stagehand.dev/v4/basics/act)). A domain policy with blocked domains appears in the caching example. Whether it applies to redirects is not documented (unverified).
- Safety: variables are substituted locally, so the model sees only placeholders ([act](https://docs.stagehand.dev/v4/basics/act)). Caveat: server-side caching sends variable values to the cache service, so credential calls need `cache: false` ([act](https://docs.stagehand.dev/v4/basics/act), [caching](https://docs.stagehand.dev/v4/best-practices/caching)). For critical actions such as deleting an account, the docs recommend observe, validate, then act ([observe](https://docs.stagehand.dev/v4/basics/observe)). No injection defense is documented.
- Accuracy: evals are at stagehand.dev/evals. Not fetched (unverified). The docs say accuracy depends on instruction specificity and recommend previewing with `observe` ([observe](https://docs.stagehand.dev/v4/basics/observe)).
- Design notes. Copy: observe then replay an Action (this is our `find ai` returning a ref), validate before acting on critical labels, and a cache keyed on instruction plus page content. Avoid: default-on server caching for calls that carry values.

### Skyvern

Sources: [README](https://raw.githubusercontent.com/Skyvern-AI/skyvern/main/README.md), [2.0 post](https://www.skyvern.com/blog/skyvern-2-0-state-of-the-art-web-navigation-with-85-8-on-webvoyager-eval/), [HTML vs JSON post](https://www.skyvern.com/blog/html-vs-json-llm-tokens-cost-reduction-success-rate/), [3.0 post](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/).

- Targeting: hybrid. In 2.0 the model gets an annotated screenshot plus a text list of interactable elements ([HTML vs JSON](https://www.skyvern.com/blog/html-vs-json-llm-tokens-cost-reduction-success-rate/)). In 3.0 it has tools to read HTML through model-written JavaScript, act, screenshot and finish ([3.0](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/)). Element-list caching is not documented.
- Control loop: 2.0 is a planner, actor and validator ([2.0](https://www.skyvern.com/blog/skyvern-2-0-state-of-the-art-web-navigation-with-85-8-on-webvoyager-eval/)). 3.0 is one loop with four tools. Screenshots fell from 27% to 2.7% of LLM calls ([3.0](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/)). I found no run cap in the pages read.
- Speed (vendor production A/B, about 100,000 runs): mean run time 593 s (2.0) vs 262 s (3.0); cost per run $0.039 vs $0.0302; LLM calls per run 6.4 vs 23; tokens per run 188K vs 286K ([3.0](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/)). The post says image-bearing calls encode about 1.7x slower, citing arXiv 2607.23373 (unverified). No per-call latency is published.
- Navigation: 2.0's planner handles multi-step goals. The 3.0 action list includes tab switching ([3.0](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/)). I found no domain allowlist.
- Safety: Bitwarden and custom HTTP credential integrations, and TOTP 2FA ([README](https://raw.githubusercontent.com/Skyvern-AI/skyvern/main/README.md)). Telemetry is on by default and can be turned off with `SKYVERN_TELEMETRY=false` ([README](https://raw.githubusercontent.com/Skyvern-AI/skyvern/main/README.md)). No injection or confirmation policy found.
- Accuracy: WebVoyager 85.85% for 2.0 with GPT-4o and GPT-4o-mini on Skyvern Cloud, after removing 8 tasks. The planner alone reached about 68.7% and the first loop about 45% ([2.0](https://www.skyvern.com/blog/skyvern-2-0-state-of-the-art-web-navigation-with-85-8-on-webvoyager-eval/)). 3.0 reports 90.5% on Odysseys with 65.4 average steps (vendor, Opus 5). WebBench 64.4% ([README](https://raw.githubusercontent.com/Skyvern-AI/skyvern/main/README.md)).
- Design notes. Copy: the validator role (it caught false success), HTML over JSON for element lists (about 1,100 tasks: success 59.9% to 63.8%, median cost on successful tasks $1.22 to $1.08, [HTML vs JSON](https://www.skyvern.com/blog/html-vs-json-llm-tokens-cost-reduction-success-rate/)). Avoid: a planner loop for simple resolution. Skyvern says it is "exploring" Jev-class models but notes today's models have a 64K context and no image input ([3.0](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/)).

### Playwright MCP (Microsoft)

Sources: [README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md).

- Targeting: accessibility snapshot. Actions take "exact target element reference from the page snapshot, or a unique element selector". `browser_find` searches the snapshot. Vision needs the `vision` capability ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)).
- Control loop: no model inside the server. The MCP client's model picks each tool call, and each call is one deterministic action. `timeout-settle` (default 500 ms) waits for triggered work after each action ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)). The server has no step cap.
- Speed: no model time on the server. The README says CLI plus skills is more token-efficient than MCP for coding agents ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)). No latency numbers.
- Navigation: `browser_tabs` lists, creates, closes and selects tabs. `--allowed-origins` "does not serve as a security boundary and does not affect redirects" ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)).
- Safety: "Playwright MCP is not a security boundary" ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)). `--secrets` replaces matching text in responses and is "a convenience and not a security feature" ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)). `browser_run_code_unsafe` runs arbitrary JavaScript in the server process ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)).
- Accuracy: none published.
- Design notes. Copy: snapshot-bound refs and the settle timer. Avoid: an origin allowlist that ignores redirects, and redaction presented as a security control.

### Anthropic: computer use and browser use tool

Sources: [computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool), [browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool).

- Targeting: computer use is screenshots and pixel coordinates. The browser tool returns refs from `read_page` and `find` (accessibility tree), with coordinates as fallback. Refs are scoped to a tab and stay valid "until that tab navigates or its DOM changes materially. The API can't detect a stale or unknown reference", so the executor must return an error ([browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool)).
- Control loop: one model turn returns a batch of tool calls. The executor runs them in order, stops at the first failure, and answers the rest with "Not executed" ([computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool), [browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool)). The sample loop caps iterations (`max_iterations`, 10 in the example, [computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)).
- Speed: no latency published. The documented lever is batching, which "saves a round trip" ([computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)).
- Navigation: tab members `new_tab`, `switch_tab`, `close_tab` and `list_tabs` ([browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool)). The docs recommend a domain allowlist enforced at the network layer and re-checked after redirects, and a scheme check in the navigate handler ([browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool)).
- Safety: treat all page content as untrusted ([browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool)). Classifiers scan computer-use tool results for injection, with an opt-out ([computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)). Human confirmation for consequential actions is the developer's responsibility ([both](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)).
- Accuracy: none in these docs.
- Design notes. Copy: the stale-ref error, the halt-on-first-failure batch rule, scheme checks with a URL parser, and redirect-aware allowlists. The docs also say Claude "sometimes acts as if its actions succeeded" and advise a screenshot after each step ([computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)). Avoid: pixel coordinates when a tree exists, which is their own advice.

### Claude in Chrome (Anthropic extension)

Sources: [permissions guide](https://support.claude.com/en/articles/12902446-claude-in-chrome-permissions-guide), [admin controls](https://support.claude.com/en/articles/13065128-claude-in-chrome-admin-controls), [product page](https://claude.com/claude-in-chrome).

- Targeting: not documented. The help page says the `debugger` permission lets Claude click, type and take screenshots ([install permissions](https://support.claude.com/en/articles/12012173-get-started-with-claude-in-chrome)).
- Control loop: an agent in the side panel with three modes. Manual asks before each action. Auto runs a safety review on each action. Skip runs no checks ([permissions guide](https://support.claude.com/en/articles/12902446-claude-in-chrome-permissions-guide)).
- Speed: none published.
- Navigation: the classic panel shows a plan listing the sites Claude may use, and other sites need approval. Organizations set allowlists and blocklists. The default blocked categories include financial services, crypto exchanges, adult content and pirated content ([permissions](https://support.claude.com/en/articles/12902446-claude-in-chrome-permissions-guide), [admin](https://support.claude.com/en/articles/13065128-claude-in-chrome-admin-controls), [troubleshooting](https://support.claude.com/en/articles/12902405-claude-in-chrome-troubleshooting)).
- Safety: prohibited in every mode: purchases, account creation, sensitive credit card or ID data, downloads from untrusted sources, and completing instructions from emails or web content ([permissions guide](https://support.claude.com/en/articles/12902446-claude-in-chrome-permissions-guide)). Even under "Always allow", downloads, sensitive information entry and granting authorizations still need approval ([permissions guide](https://support.claude.com/en/articles/12902446-claude-in-chrome-permissions-guide)).
- Accuracy: none published.
- Design notes. Copy: the three-tier mode model, per-site approval, and the protected-action list. Avoid: a skip-all mode in any path we ship.

### OpenAI: CUA and Operator

Sources: [CUA post](https://openai.com/index/computer-using-agent/), [Operator post](https://openai.com/index/introducing-operator/), [ChatGPT agent post](https://openai.com/index/introducing-chatgpt-agent/), [help center](https://help.openai.com/en/articles/11752874-chatgpt-agent).

- Targeting: screenshots, mouse and keyboard only ([CUA](https://openai.com/index/computer-using-agent/)).
- Control loop: one model loop. It perceives a screenshot, reasons, acts, and repeats until done or user input is needed ([CUA](https://openai.com/index/computer-using-agent/)).
- Speed: no per-step latency. OSWorld success rises with the number of allowed steps ([CUA](https://openai.com/index/computer-using-agent/)).
- Navigation: a blocklist of sites and a watch mode for sensitive sites ([CUA](https://openai.com/index/computer-using-agent/)).
- Safety: trained to ask for confirmation "before finalizing tasks with external side effects". A monitor model pauses on suspicious content. The model "is designed to identify and ignore prompt injections", and recognized all but one case in early red-teaming ([CUA](https://openai.com/index/computer-using-agent/)).
- Accuracy: OpenAI-reported, January 2025: OSWorld 38.1%, WebArena 58.1%, WebVoyager 87% ([CUA](https://openai.com/index/computer-using-agent/)). Independent human evaluation on Online-Mind2Web: Operator 61.3% ([paper](https://arxiv.org/html/2504.01382v4)).
- Status: Operator was merged into ChatGPT agent on July 17, 2025 ([Operator post](https://openai.com/index/introducing-operator/)). The ChatGPT agent help page now says the agent "is no longer available" and points to ChatGPT Work ([help center](https://help.openai.com/en/articles/11752874-chatgpt-agent)). The page's update date was not shown, so treat this as the status as of the fetch (unverified beyond that).
- Design notes. Copy: confirmation before external side effects, trained into the model and backed by a monitor. Avoid: pixel-only control as the sole path.

### Amazon Nova Act

Sources: [user guide](https://docs.aws.amazon.com/nova-act/latest/userguide/what-is-nova-act.html), [service card](https://docs.aws.amazon.com/pdfs/ai/responsible-ai/nova-act/nova-act.pdf) (excerpt), [SDK README](https://github.com/aws/nova-act?tab=readme-ov-file) (excerpt), [Amazon Science](https://www.amazon.science/blog/nova-act) (excerpt), [AWS blog](https://aws.amazon.com/blogs/machine-learning/amazon-nova-act-sdk-preview-path-to-production-for-browser-automation-agents/) (excerpt).

- Targeting: screenshots to a multimodal model. Actions run through Playwright, and `nova.page` exposes the Playwright page for DOM and screenshots ([user guide](https://docs.aws.amazon.com/nova-act/latest/userguide/what-is-nova-act.html)).
- Control loop: a service-side ReAct loop. Each step sends a screenshot and the prompt, and each step's output passes guardrail validation before the SDK executes it ([service card](https://docs.aws.amazon.com/pdfs/ai/responsible-ai/nova-act/nova-act.pdf)). `act()` defaults to `max_steps` 30. The SDK README advises fewer than 30 steps and staging longer tasks in code (excerpt, [SDK README](https://github.com/aws/nova-act?tab=readme-ov-file)).
- Speed: no per-step latency. The SDK recommends a `timeout` over `max_steps` because "time per step can vary" (excerpt, [SDK README](https://github.com/aws/nova-act?tab=readme-ov-file)).
- Navigation: multi-step inside one `act()`, with parallel sessions ([user guide](https://docs.aws.amazon.com/nova-act/latest/userguide/what-is-nova-act.html)). I found no domain allowlist.
- Safety: warns that third-party content can carry prompt injections, advises against passwords, notes screenshots can capture sensitive values, and advises monitoring ([service card](https://docs.aws.amazon.com/pdfs/ai/responsible-ai/nova-act/nova-act.pdf)).
- Accuracy: "scoring >90% on internal evals" ([Amazon Science](https://www.amazon.science/blog/nova-act), March 2025) and "over 90% reliability across early enterprise workflows" ([AWS blog](https://aws.amazon.com/blogs/machine-learning/amazon-nova-act-sdk-preview-path-to-production-for-browser-automation-agents/), July 2025). Vendor-only. The named benchmarks (ScreenSpot, GroundUI Web) have no numbers I could extract (unverified).
- Design notes. Copy: an explicit step budget and timeout, a guardrail on each step output, and staging long tasks in code. Avoid: one long prompt for a whole workflow.

### Magnitude

Sources: [repository README](https://github.com/magnitudedev/browser-agent) (excerpt; the raw file URL returned 404), [docs index](https://docs.magnitude.run/llms.txt) (excerpt).

- Targeting: vision only. "Visually grounded LLM specifies pixel coordinates", and the README argues against numbered boxes ([README](https://github.com/magnitudedev/browser-agent)). Extraction reads DOM content against a Zod schema ([README](https://github.com/magnitudedev/browser-agent)).
- Control loop: `agent.act` takes a natural-language description and runs "one or more" actions ([BrowserAgent docs](https://docs.magnitude.run/reference/browser-agent)). Caching is "in progress" ([README](https://github.com/magnitudedev/browser-agent)).
- Speed: none published.
- Navigation: `nav(url)`. I found no allowlist.
- Safety: the `data` option fills placeholders. Whether values reach the model is not stated (unverified).
- Accuracy: "state-of-the-art, scoring 94% on WebVoyager" ([README](https://github.com/magnitudedev/browser-agent)). The model, date and judge are not in the excerpt (unverified). The README recommends Claude Sonnet 4 ([README](https://github.com/magnitudedev/browser-agent)).
- Design notes. Avoid vision-only as the default. Copy: schema-driven extraction with `.describe()` hints.

### LaVague

Sources: [README](https://raw.githubusercontent.com/lavague-ai/LaVague/main/README.md).

- Targeting: DOM chunks retrieved for each step, which the telemetry list calls "source nodes", and driver code generated for Selenium or Playwright ([README](https://raw.githubusercontent.com/lavague-ai/LaVague/main/README.md)).
- Control loop: a planner and an executor. The "World Model" maps objective and page to instructions. The "Action Engine" compiles them to driver code ([README](https://raw.githubusercontent.com/lavague-ai/LaVague/main/README.md)). I found no step cap.
- Speed: no latency numbers. A token counter exists ([README](https://raw.githubusercontent.com/lavague-ai/LaVague/main/README.md)).
- Navigation: iframes work on Selenium and Playwright. Several tabs work on Selenium and the extension, and are "coming soon" on Playwright ([README](https://raw.githubusercontent.com/lavague-ai/LaVague/main/README.md)).
- Safety: telemetry is on by default and includes the objective, generated action code, URLs and page chunks. Opt out with `LAVAGUE_TELEMETRY=NONE` ([README](https://raw.githubusercontent.com/lavague-ai/LaVague/main/README.md)). Generated code runs in the driver. I found no allowlist.
- Accuracy: none in the README.
- Design notes. Avoid: generated code as the action format for a CLI agent, and default telemetry that includes page content.

### Notte

Sources: [README](https://raw.githubusercontent.com/nottelabs/notte/main/README.md), [act, choose and check docs](https://docs.notte.cc/features/sessions/act).

- Targeting: the observed page is turned into multiple-choice options. "The decider never writes selectors or actions" ([act docs](https://docs.notte.cc/features/sessions/act)). Elements are named by accessible name by default, and only viewport elements are offered ([act docs](https://docs.notte.cc/features/sessions/act)).
- Control loop: `act()` runs one observe, decide, execute step. `choose()` and `check()` ask typed questions about the page ([act docs](https://docs.notte.cc/features/sessions/act)). The loop with a step budget and repeat guard is user code shown in the docs. Notte removed its built-in agent loop in 1.10 ([README](https://raw.githubusercontent.com/nottelabs/notte/main/README.md)).
- Speed: each step returns `timings` in milliseconds for observe, decide, write, fresh-page check and execute ([act docs](https://docs.notte.cc/features/sessions/act)). No published numbers found.
- Navigation: `goto` through `execute`. Scrolling is a decision ([act docs](https://docs.notte.cc/features/sessions/act)). Tabs and allowlists are not documented.
- Safety: "an answer that is not one of the offered options executes nothing". Invalid answers are asked once more, then fail ([act docs](https://docs.notte.cc/features/sessions/act)). A fresh-page check runs before execution and returns `stale` if the target changed. `min_confidence` and `dry_run` are available. Variables are substituted after the model call, and vault values are masked in later model requests ([act docs](https://docs.notte.cc/features/sessions/act)).
- Decider: Jev through OpenRouter (an alpha endpoint), Jev through TypeSafe's API, or an LLM. With an LLM decider the probabilities are 1.0 or 0.0, so `check()` is not calibrated. Jev cannot write text, so a separate writer LLM does ([act docs](https://docs.notte.cc/features/sessions/act)).
- Accuracy: none published.
- Design notes. The closest match to our slice. Copy: strict option validation, dry run, `min_confidence`, the stale status, and a caller-owned loop with a repeat guard. Watch: the Jev route depends on an alpha endpoint, so it keeps an LLM fallback.

### jev-ultrafast (browser-use, Jev-driven)

Sources: [repository](https://github.com/browser-use/jev-ultrafast), [performance report](https://raw.githubusercontent.com/browser-use/jev-ultrafast/main/docs/performance.md).

- Targeting: a DOM reader over common HTML and ARIA controls, a numbered element table, and no screenshots in the default loop ([README](https://github.com/browser-use/jev-ultrafast)).
- Control loop: one goal, given once. Each cycle makes one Jev request with an operation head and a target head. A small LLM writes text only for `TYPE_TEXT` ([README](https://github.com/browser-use/jev-ultrafast)). "Two decisions, one network round trip". Model output never becomes selectors, coordinates or JavaScript, and the executor re-resolves the target ([README](https://github.com/browser-use/jev-ultrafast)).
- Speed (one task, three matched pairs, Google Flights): median task time 9.45 s to 7.09 s. Median Jev latency 178 ms over 17 requests. Browser protocol calls 1,092 to 101. The report itself says "Three pairs are too few for a strong statistical claim" ([performance](https://raw.githubusercontent.com/browser-use/jev-ultrafast/main/docs/performance.md)).
- Navigation: pop-up tabs are unsupported, and owned tabs share the Chrome profile ([README](https://github.com/browser-use/jev-ultrafast)).
- Safety: click guards compare the target, form values and nearby context, and check occlusion before input. DONE needs independent verification ([README](https://github.com/browser-use/jev-ultrafast), [performance](https://raw.githubusercontent.com/browser-use/jev-ultrafast/main/docs/performance.md)). Destructive-action gating is not documented.
- Accuracy: one task, both arms 3/3 ([performance](https://raw.githubusercontent.com/browser-use/jev-ultrafast/main/docs/performance.md)). Not a benchmark.
- Design notes. Copy: operation and target heads in one request, click guards, and independent DONE checks. Cost check: 90,558 input tokens across 17 requests, about 5,300 per request ([performance](https://raw.githubusercontent.com/browser-use/jev-ultrafast/main/docs/performance.md)). At the listed $0.042 per million tokens ([jev-support.md](jev-support.md)), that is about $0.004 per task (arithmetic, not a measured bill).

### TypeSafe Jev (vendor claims)

Sources: [launch post](https://typesafe.ai/blog/introducing-system-one-models-and-jev) (the page shows "Sep 15, 2026" and metadata shows "Nov 6, 2026"), [How to build](https://docs.typesafe.ai/concepts/how-to-build-with-system-one.md) (cited in [jev-support.md](jev-support.md)).

- Latency: docs say "most calls complete in about 100 ms". The launch post says "70ms-500ms" end to end. These disagree. [jev-support.md](jev-support.md) cites only the first.
- Claims: outputs are type-valid and "can't hallucinate"; cardinality up to 255 options, with a two-stage path above that ([launch post](https://typesafe.ai/blog/introducing-system-one-models-and-jev)). Workflow evals claim 193.6x faster and 444.6x cheaper, using averages of GPT and Claude as reference answers. The authors call these their own evals and say their numbers are "on the higher end" (vendor, [launch post](https://typesafe.ai/blog/introducing-system-one-models-and-jev)).
- Limits from the docs: reads literally, leans toward the first option, and accuracy falls with irrelevant detail ([jev-support.md](jev-support.md), citing [model jaggedness](https://docs.typesafe.ai/model-jaggedness.md)). Type validity is not the same as choosing the right option.

### agent-browser (this repo, baseline)

Sources: [README](https://raw.githubusercontent.com/vercel-labs/agent-browser/main/README.md), [security](../content/docs/security.mdx) (local).

- Targeting: accessibility-snapshot refs (`@eN`). Refs "describe the page at the time of the snapshot" ([README](https://raw.githubusercontent.com/vercel-labs/agent-browser/main/README.md)). A click fails when the target is hidden, disabled, outside the viewport or covered ([README](https://raw.githubusercontent.com/vercel-labs/agent-browser/main/README.md)).
- Loop: no LLM in the command loop. The caller chooses each command ([README](https://raw.githubusercontent.com/vercel-labs/agent-browser/main/README.md)).
- Safety: the allowlist blocks navigations, sub-resources, WebSocket, EventSource and `sendBeacon` to non-allowed domains. WebRTC is disabled in supported Chromium. Attach and profile modes reject `--allowed-domains` ([security.mdx](../content/docs/security.mdx)). The action policy and `--confirm-actions` gate eval, download and upload, with a 60-second auto-deny ([security.mdx](../content/docs/security.mdx)).

## Benchmarks

| Benchmark | Source | What it measures | Reported results | Caveats |
| --- | --- | --- | --- | --- |
| WebVoyager | [arXiv 2401.13919](https://arxiv.org/abs/2401.13919) (ACL 2024) | 15 live websites, GPT-4V-based judge | 59.1% task success; judge 85.3% agreement with humans (paper) | A search-only agent solves 51% of WebVoyager tasks and 22% of Online-Mind2Web tasks ([paper](https://arxiv.org/html/2504.01382v4)). Task count not checked. |
| Online-Mind2Web | [arXiv 2504.01382](https://arxiv.org/abs/2504.01382) (COLM 2025; v4 Oct 2025) | 300 tasks, 136 websites, human-judged | Human eval: Operator 61.3%, Claude Computer Use 3.7 56.3%, SeeAct 30.7%, Claude Computer Use 3.5 29.0%, Browser Use 30.0%, Agent-E 28.0% ([paper](https://arxiv.org/html/2504.01382v4)) | Judge (WebJudge) agreement 83.6% to 85.7% on average. Failed runs take about twice as many steps as successful ones for three agents. Dataset access is gated on Hugging Face ([BU Bench README](https://raw.githubusercontent.com/browser-use/benchmark/main/README.md)). |
| WebArena | [arXiv 2307.13854](https://arxiv.org/abs/2307.13854) | Self-hosted sites in four domains, functional checks | Best GPT-4 agent 14.41% vs human 78.24% (paper, v4) | 2023 agents. OpenAI reports 58.1% for CUA (vendor, [CUA](https://openai.com/index/computer-using-agent/)). |
| Vendor and internal | Vendor posts | Varies | Skyvern 2.0 85.85% WebVoyager; Skyvern 3.0 90.5% Odysseys; Magnitude 94% WebVoyager; OpenAI CUA 87% WebVoyager (Jan 2025); Browser Use Bench Hard V4 76% (106 tasks); Nova Act ">90%" internal evals | Not reproducible from the sources. Models, judges and task sets differ. |

## Speed by pattern

| Pattern | Model calls per action | Published latency | Source |
| --- | --- | --- | --- |
| Deterministic ref or selector | 0 | Playwright MCP waits 500 ms after each action by default | [README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md) |
| Replay of an observed Action | 0 | No timing published | [act](https://docs.stagehand.dev/v4/basics/act) |
| Cache hit | 0 | No timing published; "instantly" | [caching](https://docs.stagehand.dev/v4/best-practices/caching) |
| Single Jev Choice | 1 | About 100 ms (docs); 70 to 500 ms (launch post); 178 ms median in one run | [How to build](https://docs.typesafe.ai/concepts/how-to-build-with-system-one.md), [launch post](https://typesafe.ai/blog/introducing-system-one-models-and-jev), [performance](https://raw.githubusercontent.com/browser-use/jev-ultrafast/main/docs/performance.md) |
| Text-only LLM turn (Skyvern 3.0) | 1 per turn; 23 turns per run on average | 262 s mean per run in production; no per-turn number | [3.0](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/) |
| Vision turn (CUA, Anthropic computer use, Magnitude) | 1 per turn | None published | [CUA](https://openai.com/index/computer-using-agent/), [computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool), [README](https://github.com/magnitudedev/browser-agent) |
| Full task, hard (browser-use V4) | Many | About 5 min median in production; about 10 min on hard tasks | [choosing an agent](https://docs.browser-use.com/cloud/choosing-an-agent.md) |

Per-run times include browser work, so I did not derive per-turn model times from them.

## Comparison

### Where the proposal matches the field

- Typed decision over offered options, validated before execution. Notte's `act()` and `jev-ultrafast` both do this with Jev ([act docs](https://docs.notte.cc/features/sessions/act), [README](https://github.com/browser-use/jev-ultrafast)).
- Deterministic execution after the decision, with freshness and occlusion checks. Notte checks the page before executing ([act docs](https://docs.notte.cc/features/sessions/act)). `jev-ultrafast` runs click guards ([README](https://github.com/browser-use/jev-ultrafast)). agent-browser already fails clicks on covered targets ([README](https://raw.githubusercontent.com/vercel-labs/agent-browser/main/README.md)).
- Caller-owned loop with a cap and repeat guard. Notte's documented loop does this ([act docs](https://docs.notte.cc/features/sessions/act)), and Nova Act caps steps at 30 by default (excerpt, [SDK README](https://github.com/aws/nova-act?tab=readme-ov-file)).
- Refs bound to a snapshot, with stale errors. Anthropic's browser tool and Playwright MCP work this way ([browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool), [README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)).
- Confirmation before consequential actions, and untrusted page content. OpenAI and Claude in Chrome both do this ([CUA](https://openai.com/index/computer-using-agent/), [permissions](https://support.claude.com/en/articles/12902446-claude-in-chrome-permissions-guide)).

### Where it differs

- Operation. Our `click --ai` decides only the target. Notte and `jev-ultrafast` decide the operation and the target together, which goal navigation needs for `type` and `select` ([act docs](https://docs.notte.cc/features/sessions/act), [README](https://github.com/browser-use/jev-ultrafast)).
- Candidate scope. Our plan offers every snapshot ref. Notte offers viewport elements only ([act docs](https://docs.notte.cc/features/sessions/act)). `jev-ultrafast` offers only supported operations and targets ([README](https://github.com/browser-use/jev-ultrafast)).
- Text. We need none. Notte and `jev-ultrafast` use a second LLM for text, which adds a second vendor and another egress path ([act docs](https://docs.notte.cc/features/sessions/act), [README](https://github.com/browser-use/jev-ultrafast)).
- Calibration. Notte's LLM decider returns 1.0 or 0.0, so thresholds only mean something with a calibrated source such as Jev ([act docs](https://docs.notte.cc/features/sessions/act)). Our thresholds depend on Jev's calibration, which is not yet shown on web candidates.
- Loop ownership. Our (b) is a CLI loop. Notte's loop is user code. `jev-ultrafast` is a Jev-owned agent that gets one goal. Our plan sits between the first two.

### Proven and unproven

- Proven in shipped products: a typed per-step decision over a closed set; deterministic execution after it; stale checks; step caps; confirmation for destructive actions; validate-before-act for critical labels (sources above).
- Partly proven: Jev as the decider. Two third-party projects ship it. Each has one set of timings from a single task. No project publishes accuracy on web candidate lists.
- Unproven: a judgment-only Choice replacing the planner for multi-hop goals. Skyvern moved from planner and validator to a single loop, but still runs a per-step LLM with tools and more turns ([3.0](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/)). That is evidence about loop shape, not about a resolver with no planner.
- Also unproven: Noul as an outcome check on web pages (no web numbers published); pagination and tab-by-intent (no precedent found).

### Failure modes and field responses

| Failure | Where it was seen | Field response | Our slice |
| --- | --- | --- | --- |
| Plausible but wrong element | Stagehand troubleshooting ([observe](https://docs.stagehand.dev/v4/basics/observe)); Operator's errors were mostly filters and sorting, 57.7% ([paper](https://arxiv.org/html/2504.01382v4)) | Fewer, actionable options; check target name and context before input; dry run | Dry run, `min_confidence`, offered refs only, target re-check before click |
| Believes it succeeded when it failed | Skyvern's planner-only version reported success on clicks that had not added the item; a validator was added in 2.0 ([2.0](https://www.skyvern.com/blog/skyvern-2-0-state-of-the-art-web-navigation-with-85-8-on-webvoyager-eval/)); Claude "sometimes acts as if its actions succeeded" ([computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)) | Independent validator; DONE not trusted without a check ([jev-ultrafast](https://github.com/browser-use/jev-ultrafast)) | Not in the slice. Needs a deterministic post-check (URL or text). Noul only as a hint. |
| Page changed between decision and action | Notte `stale` status ([act docs](https://docs.notte.cc/features/sessions/act)); Anthropic stale refs ([browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool)) | Re-check the fingerprint; decide once more; return `stale` | Tie each ref to its snapshot; reject refs from older snapshots |
| Repeated actions and runaway steps | Failed Online-Mind2Web runs take about twice the steps of successful ones ([paper](https://arxiv.org/html/2504.01382v4)) | Step cap; repeat guard ([Notte](https://docs.notte.cc/features/sessions/act)) | Cap flag; repeat guard on (ref, page fingerprint) |
| Injected text steers the choice | OpenAI monitor and training ([CUA](https://openai.com/index/computer-using-agent/)); classifiers ([computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)); pages hide instructions ([Claude in Chrome](https://claude.com/claude-in-chrome)) | Limit to offered options, so the worst case is a wrong ref; gate destructive actions | Already true for offered refs. Keep the destructive gate. |
| Redirect leaves the allowlist | Playwright MCP's origin lists do not cover redirects ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)); Anthropic recommends re-checking after redirects ([browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool)) | Check every hop | Add a redirect test for `--allowed-domains` and for Jev egress |
| Pixel misclick | Pixel targeting in Magnitude and CUA ([README](https://github.com/magnitudedev/browser-agent), [CUA](https://openai.com/index/computer-using-agent/)); Anthropic advises zoom for small text | Prefer refs; zoom | Refs avoid this |
| Option order bias | [model jaggedness](https://docs.typesafe.ai/model-jaggedness.md) (via [jev-support.md](jev-support.md)) | Reorder and check consistency | Deterministic DOM order plus a reversed-order check near the threshold |
| Oversized or irrelevant candidate list | Notte offers viewport elements only ([act docs](https://docs.notte.cc/features/sessions/act)); accuracy falls with irrelevant detail ([jev-support.md](jev-support.md)) | Viewport or actionable only | Visible, actionable candidates only; cap the count |

### Fastest per action, and the accuracy cost

The fastest per action is any zero-model path: a deterministic ref, a replayed action, or a cache hit. Among model-in-loop patterns, a single Choice is the only one with a published per-call number (about 100 ms in docs, 178 ms median in one run). Vision and planner loops publish only per-run times.

The accuracy cost of the zero-model paths is that they cannot recover from a changed page without a model. They need a freshness check and a fallback, which is what Stagehand's `selfHeal`, Anthropic's stale-ref error and Notte's `stale` status provide.

The single Choice adds a per-decision error rate. No public source measures that rate on web candidate lists. Notte and `jev-ultrafast` rely on guards, not published accuracy.

The vision and planner loops show the highest published accuracy, and they get it from validation and replanning, which a per-action resolver does not have. Inference, not measured here: the caller's own turns (a frontier model choosing the next goal step) probably dominate end-to-end time, so per-action speed matters only after those turns are fast.

### Adopt (5)

1. A strict decision contract for `find ai` and `click --ai`. Return one offered ref or none. An answer outside the offered set executes nothing. Add `--dry-run` (returns the choice and probabilities), a confidence gate, and a target re-check before the click. Sources: [Notte act docs](https://docs.notte.cc/features/sessions/act), [jev-ultrafast](https://github.com/browser-use/jev-ultrafast).
2. Candidate rules. Offer visible, actionable elements (roles such as link, button and textbox), each with an accessible name and an index for duplicates. Cap the count and keep DOM order. Test a reversed order on near-threshold answers. Sources: [Notte act docs](https://docs.notte.cc/features/sessions/act), [model jaggedness](https://docs.typesafe.ai/model-jaggedness.md).
3. A caller-loop contract for (b): explicit goal, a step cap, a repeat guard on (ref, page fingerprint), and a stop reason of `done`, `blocked` or `stale`. Sources: [Notte loop](https://docs.notte.cc/features/sessions/act), [Nova Act](https://github.com/aws/nova-act?tab=readme-ov-file), [browser-use max_steps](https://docs.browser-use.com/open-source/customize/agent/basics.md).
4. Destructive labels return a `confirmation_required` result instead of acting, and reuse the existing `--confirm-actions` categories. Validate before acting on critical labels. Sources: [Stagehand observe](https://docs.stagehand.dev/v4/basics/observe), [Claude in Chrome](https://support.claude.com/en/articles/12902446-claude-in-chrome-permissions-guide).
5. Add a redirect-aware test for `--allowed-domains` and apply the same check to Jev egress. Sources: [Playwright MCP caveat](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md), [Anthropic browser use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/browser-use-tool).

### Reject (5)

1. A hidden planner inside `find ai` or `click --ai`, such as `run_task` or a built-in agent loop. It conflicts with the caller owning the goal and hides cost. Notte removed its loop in 1.10 ([README](https://raw.githubusercontent.com/nottelabs/notte/main/README.md)).
2. Vision-first defaults (Magnitude, OpenAI CUA, Nova Act's screenshot loop). Refs are cheaper and deterministic. Skyvern cut screenshot calls to 2.7% and reported lower cost and time (vendor, [3.0](https://www.skyvern.com/blog/deleting-rag-from-our-web-agent-made-it-2-3x-faster/)).
3. Origin-only allowlists that skip redirects (Playwright MCP's semantics) ([README](https://raw.githubusercontent.com/microsoft/playwright-mcp/main/README.md)).
4. Default-on server caching for calls that carry values or credentials (Stagehand's caveat, [act](https://docs.stagehand.dev/v4/basics/act)). Cache keys must exclude secrets, and credential fields should not be cached.
5. Multi-step instruction strings and vendor numbers as acceptance criteria. Stagehand calls multi-step instructions "unreliable" ([act](https://docs.stagehand.dev/v4/basics/act)). Vendor latency (100 ms, 70 to 500 ms, 193.6x) and "can't hallucinate" are not our tests. Measure on our snapshots.

## Corrections and additions to jev-support.md

- Latency: add the 70 to 500 ms range from the launch post, and the measured 178 ms median from one run ([performance](https://raw.githubusercontent.com/browser-use/jev-ultrafast/main/docs/performance.md)). Note the disagreement with the docs' "about 100 ms".
- Prior art: add Notte's `act()` ([docs](https://docs.notte.cc/features/sessions/act)) and `jev-ultrafast` ([repo](https://github.com/browser-use/jev-ultrafast)) to the candidate-feature analysis. Both confirm the strict-answer, dry-run and fresh-page design.
- Operation head: both precedents decide operation and target together. The current note covers click only.
- Egress: the Notte Jev route uses an alpha OpenRouter endpoint. Record this in the vendor-risk section.

## Gaps and unverified items

- Claude in Chrome targeting method: not documented.
- Pagination and tab-by-intent: no precedent found in any tool.
- Run-level step caps: not found for Stagehand, Skyvern, LaVague, Notte, Magnitude or Playwright MCP (the server has none).
- Domain allowlists: not found for Skyvern, Nova Act, Notte, LaVague or Magnitude. Claude in Chrome has org-level allowlists only. Stagehand's policy and redirects are unverified.
- Stagehand evals (stagehand.dev/evals) were not fetched.
- Magnitude: read from a search excerpt. The raw README URL returned 404. The model and date behind the 94% figure are unverified.
- Nova Act: the user guide was fetched in full. The SDK README and service card were read as excerpts.
- OpenAI CUA's evaluation PDF was not fetched. The ChatGPT agent help page's update date was not shown.
- WebVoyager task count: not in the abstract, not checked.
- browser-use's claim that the element index is the CDP backend node ID: from a third-party guide, not verified in code.
- BU Bench V2.1 scores: in images, not extracted.
- Notte's `act()` accuracy and its per-step timings: not published.
- jev-ultrafast cost figure: arithmetic from the listed price and the reported token count, not a measured bill.
- TypeSafe launch post date: the page shows Sep 15, 2026 and the metadata shows Nov 6, 2026.

## Open questions for the user

1. Operation head in the first slice. Notte and `jev-ultrafast` decide operation and target together. Should `click --ai` cover `type` and `select` from the start, or stay click-only?
2. Candidate scope: all snapshot refs (current plan) or viewport-visible refs only (Notte)?
3. Go or no-go data. No public source gives Jev's accuracy on web candidates. Will our 50 to 100 saved snapshots be the decision set, or do you also want Online-Mind2Web (300 tasks, gated access)?
4. Latency budget. Is there an acceptance target for added per-action latency? The measured median is 178 ms from one run, and the vendor gives 70 to 500 ms or about 100 ms.
5. Ownership of loop (b). A CLI command like `jev-ultrafast`, or a documented recipe the caller runs, like Notte?
6. Verification. Should each goal step require a deterministic post-check, with Noul only as a hint, or should Noul be allowed as a stop condition?
7. Should Jev calls obey `--allowed-domains`? This is open question 7 in [jev-support.md](jev-support.md). It matters more here because Notte and `jev-ultrafast` both send page state to Jev.
8. Text entry later. Accept a second LLM for text, as Notte and `jev-ultrafast` do, or keep `fill` deterministic?
