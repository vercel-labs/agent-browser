# v3 pre-registration

Written and committed before any v3 run. The rule, the checks and the token counting below do not change after results are seen. A harness bug is fixed, every affected variant is re-run, and the bug is reported.

## Question

Does the apc snapshot source keep task success while using a fraction of the snapshot tokens of the ax source, on real public pages as well as on local fixtures?

## Variants

- ax: the accessibility tree snapshot (`--source ax`).
- ax-deep: the same snapshot with the nested-frame expansion that apc and hybrid get. It is selected with the environment variable `AGENT_BROWSER_AX_DEEP`, set by the runner only for this variant. It is the control for the headline comparison.
- apc and hybrid: as in v2, at the commit of the run. hybrid is not a public source; the runner sets `AGENT_BROWSER_SNAPSHOT_HYBRID` for it.

Plain ax is reported too. The headline comparison is apc against ax-deep.

## Tasks

66 tasks: the 26 local tasks of v2 (20 tuned plus 6 held out) and 40 public-page tasks in `public-tasks.ts`, spread over 17 sites. Public success is decided by the harness: final URL or hash, echoed form payload or query string, page state read from the page, or extraction answers compared with ground truth the harness computes at run time with an independent HTTP fetch (page source or API). Ground truth is recorded with each run, before and after it.

`validate-public.ts` runs a scripted solution against every public check: each check fails on the untouched page and passes after the solution. No model is involved.

Rules for public pages: no login, no account creation, no purchase, no submission to a real business or person, no personal data. Runs are sequential per site with a delay of at least two seconds. At most two sessions run at once.

A run is invalid when the site blocks automation or shows a captcha (the title or first lines of a short page match a block pattern), when the page does not open, or when the ground truth cannot be fetched. An invalid run voids the whole unit (task and repetition) for every variant and every model. A run that fails after three attempts on an infrastructure error (gateway, process) voids the unit for that model. Invalid counts are reported by site.

## Setup

- Models through the AI Gateway: `anthropic/claude-haiku-4.5`, `openai/gpt-4.1-mini` and `google/gemini-3.5-flash-lite` (a different provider, picked from the gateway model list among models tagged tool-use after a one-task smoke run; `google/gemini-2.5-flash` was tried first and never called the snapshot tool).
- 3 repetitions per task, temperature 0, 25-step cap, for all tasks. If time or budget runs out, the run stops at a repetition boundary and the analysis uses complete units only; this is reported.
- Variant order inside a unit is rotated by repetition and task, and all variants and models of a unit run back to back, so time-of-day drift on live pages reaches every variant equally.
- Snapshot tokens are counted with `o200k_base`. The metric is the sum over a run of the tokens of every snapshot output.

## Survival rule

apc survives only if all four hold:

- (a) apc success is at least ax-deep success minus 2 percentage points, on each model, over all valid units.
- (b) Mean apc snapshot tokens per run are at most 0.75 times those of ax-deep, on each model.
- (c) Success on the holdout tasks plus the public tasks, pooled across models, is at least ax-deep minus 2 percentage points.
- (d) A paired exact McNemar test on per-(task, repetition, model) outcomes, apc against ax-deep, pooled, shows no significant success loss: a loss is significant when p < 0.05 and apc fails more discordant pairs than ax-deep.

hybrid is evaluated by the same rule. The analysis also reports 95% bootstrap confidence intervals (5000 resamples, tasks as clusters, fixed seed) for the success difference and the token ratio, per model and pooled, and splits by category, site and local against public.

## Analysis

Per-category and per-site tables, the tasks where apc loses to ax-deep with the reason read from transcripts and snapshot text, and whether earlier holdout results reproduce. A real, general apc bug found in the analysis is fixed in a separate commit and the full v3 is re-run as v3b with this rule unchanged; v3 and v3b are reported side by side.

The runner records the exact tokens of every run. Results and cost stay outside the repository, in the gitignored `evals/results/`.
