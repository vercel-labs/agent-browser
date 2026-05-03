# SPA Scraping with Accessibility Refs: The Tab-Isolation Pattern

This guide documents a pattern for sustainable, ref-based scraping of Single Page Applications (SPAs) with dynamic feeds — Google Maps, Airbnb, e-commerce listings, infinite-scroll search results, and similar. The pattern was discovered while building a real-world geographic dataset (a crime/safety map for Santa Cruz, Argentina, in collaboration with the NGO *Usina de Justicia*) using `agent-browser`'s accessibility tree snapshots, and was originally proposed in [#853](https://github.com/vercel-labs/agent-browser/issues/853).

If you have ever tried to iterate over search results with `click` → `back()` and watched your refs go stale halfway through, this is the document for you.

---

## Table of Contents

1. [The Source-Tab Invariant](#the-source-tab-invariant)
2. [Ref Lifecycle in SPAs: When Refs Become Invalid](#ref-lifecycle-in-spas-when-refs-become-invalid)
3. [Pattern: Sequential Extraction](#pattern-sequential-extraction)
4. [Cold-Start Optimization](#cold-start-optimization)
5. [Tuning Waits](#tuning-waits)
6. [Applicability and Limits](#applicability-and-limits)

---

## The Source-Tab Invariant

> **Never navigate the source tab. Always open detail pages in a new tab.**

This is the core rule. In a multi-step extraction workflow, the tab holding your search results (the *source tab*) must be treated as immutable: it is taken once, snapshotted as needed, and never asked to navigate forward, back, or follow a link. All detail-page navigation happens in ephemeral tabs that are opened, read, and destroyed.

The reason is simple: refs in the accessibility tree are valid only for the current render of the DOM. If the source tab navigates — even with `back()` from a detail page — the SPA re-renders its feed and the entire ref space is reassigned. Anything you cached is now garbage.

The Source-Tab Invariant is not a performance optimization. It is a correctness requirement.

---

## Ref Lifecycle in SPAs: When Refs Become Invalid

Accessibility refs (`@e36`, `@e37`, ...) are assigned per snapshot. They are stable as long as the underlying DOM does not change. In an SPA, the DOM changes far more often than you might expect.

### What invalidates refs

The following operations re-render the feed in the source tab and reassign every ref:

- `back()` after navigating to a detail page
- Navigating the source tab to any new URL
- Programmatic state changes triggered by the SPA's own JavaScript (lazy-load on scroll, filter changes, sort changes, modal close on some apps)
- Refresh / reload of the source tab

### What does NOT invalidate refs

- Snapshots of the source tab (read-only operation)
- Operations on other tabs — including navigation, snapshots, and close
- Switching focus between tabs

### Concrete example: Google Maps search results

**Initial snapshot of the source tab:**

```
article "Hostel Siri"            [ref=e36]
article "Hotel Santa Cruz"       [ref=e37]
article "Hostel Elcira"          [ref=e38]
article "Dptos El Buen Descanso" [ref=e39]
article "Hotel Laguna Azul"      [ref=e40]
```

**After `click @e36` → detail view → `back()`:**

```
article "Hotel Patagonia"        [ref=e26]   ← NEW item appeared
article "Hotel Santa Cruz"       [ref=e27]   ← all refs reassigned
article "Hostel Siri"            [ref=e28]
article "Dptos El Buen Descanso" [ref=e29]
article "Hostel Elcira"          [ref=e30]   ← position changed
article "Hotel Laguna Azul"      [ref=e31]
```

A loop that cached `[e36, e37, e38, e39, e40]` and tried to click them in order will succeed on the first item, succeed on the second (lucky), and fail or click the wrong thing from the third onwards. This is the failure mode that motivated the tab-isolation pattern.

---

## Pattern: Sequential Extraction

The canonical workflow is:

```
snapshot the source tab
  → for each item in the feed:
      → re-snapshot the source tab (cheap, gets a fresh ref by name)
      → click the item with --new-tab
      → operate on the new tab
      → snapshot and extract
      → close the new tab
```

Two details matter:

1. **Identify items by stable property (name, title, URL fragment), not by ref.** Refs are only used at the moment of clicking.
2. **Re-snapshot the source tab before each iteration.** This is cheap and protects you from background lazy-loading that the SPA may have triggered.

### Reference implementation

Each iteration is naturally a multi-step workflow, so it composes well with the `batch` command added in v0.25.0 ([#865](https://github.com/vercel-labs/agent-browser/pull/865)). Stable tab ids from v0.26.0 ([#892](https://github.com/vercel-labs/agent-browser/pull/892)) make tab references unambiguous across the run.

```javascript
// Open the source tab with a stable label so we can address it
// unambiguously even when other tabs come and go.
ab(`tab new --label results "${searchUrl}"`);

// Initial snapshot — establishes the universe of items.
// The source tab will not navigate again for the rest of this run.
ab(`tab results`);
const initial = ab(`snapshot -i -c`);
const itemNames = extractListingRefs(initial).map(l => l.name);

for (const name of itemNames) {
  // Re-snapshot the source tab to get a fresh ref for this iteration.
  // Cheap, and resilient to background lazy-load.
  ab(`tab results`);
  const feedSnapshot = ab(`snapshot -i -c`);
  const current = extractListingRefs(feedSnapshot).find(l => l.name === name);

  if (!current) {
    // Item disappeared from the feed (rare, but possible on dynamic feeds).
    // Skip and continue rather than aborting the whole run.
    continue;
  }

  // One round-trip: open detail in a new tab, wait, snapshot, extract URL,
  // close. The source tab is untouched throughout.
  const result = ab(`batch --json "click @${current.linkRef} --new-tab" "wait 2000" "snapshot -i -c" "get url" "tab close"`);

  const data = extractPlaceDetail(result);
  saveRecord(data);

  // Source tab refs are still valid here. The feed never re-rendered.
}
```

The same loop works without `batch` — issuing each command separately — but `batch` is a measurable win on iteration-heavy runs because it amortizes process startup and socket connection overhead across all commands in a single iteration.

### Comparison of patterns

| Pattern                                                        | Item 1 | Item 2 | Item 3+        | Notes                                      |
| -------------------------------------------------------------- | ------ | ------ | -------------- | ------------------------------------------ |
| `click` → `back()`                                             | ✅     | ✅     | ❌ refs invalid | Feed re-renders on `back()`                |
| `click --new-tab` → operate on new tab → `tab close`           | ✅     | ✅     | ✅              | **Recommended.** Source tab never navigates |
| `click --new-tab` → operate → `back()` then close              | ✅     | ✅     | ❌ refs invalid | Defeats the purpose; still triggers re-render in the source tab when focus returns |

### Validated results

This pattern was used to extract structured data from Google Maps search results across 14 cities in Santa Cruz province, Argentina:

- **70+ records** in **~19 minutes**
- **Zero ref-invalidation errors**
- **~5 seconds per item** end-to-end (2 s new-tab nav + 1 s snapshot wait + 2 s detail load)
- **Refs in the source tab never changed** across the entire run

The same approach generalizes to other ref-based scraping targets where the feed is dynamic.

---

## Cold-Start Optimization

The first navigation in a fresh `agent-browser` session can stall while Chromium spins up the daemon. On slower machines or first runs of the day, this can hit the default `execSync` timeout of 60 s even though the page actually loads.

The fix is a one-line pre-warm:

```javascript
// Pre-warm the daemon. about:blank is essentially free, but it forces
// Chromium to spin up so the next real navigation hits a hot daemon.
ab("open about:blank", 30_000);

// Real navigation — fast (~3 s) instead of timing out at 60 s.
ab(`open "${searchUrl}"`);
```

This is especially worth doing in scripts that run on cron or in CI, where every first-of-the-day run would otherwise pay the cold-start cost.

---

## Tuning Waits

The exact wait values in the reference implementation are calibrated for Google Maps. Other SPAs are typically faster.

| Wait                | Maps  | Typical SPA   | Why                                                  |
| ------------------- | ----- | ------------- | ---------------------------------------------------- |
| After `--new-tab`   | 2000  | 500–1000      | Detail page render time                              |
| After `snapshot`    | 1000  | 200–500       | Lets late-arriving DOM settle before reading refs    |
| After `tab close`   | 500   | 200           | Tab cleanup                                          |

Recommended approach for tuning a new target:

1. Start with Maps-level waits (conservative).
2. Run a 5-item extraction and confirm it works end-to-end.
3. Halve each wait one at a time. If the extraction still works, keep the lower value. If it fails, restore the previous value.
4. Stop when further reductions cause flakiness.

A flaky extraction is much more expensive than a slow one. When in doubt, keep the wait.

---

## Applicability and Limits

### This pattern fits

- SPAs with dynamic search feeds: Google Maps, Airbnb, Booking, e-commerce category pages
- Accessibility-tree-based scraping (refs rather than CSS selectors)
- Sequential multi-item extraction loops
- Any scenario where refs are "live" — valid only while the source page stays in its current render state

### This pattern is unnecessary for

- Static-content scrapers (server-rendered pages with stable markup)
- CSS-selector-based scrapers (selectors are not invalidated by re-render)
- Single-item extraction (no sequential ref lifecycle to manage)

### Known caveats

- **Detail tabs with their own SPA navigation:** if you click around inside the detail tab, that's fine — the source tab is unaffected. But do not call `back()` in the detail tab if it would land you on the source tab's URL; close the tab and return cleanly instead.
- **Feed-mutating side effects:** some SPAs mutate the feed when you hover, focus, or click — even with `--new-tab`. If you see ref drift despite using this pattern, instrument with a snapshot diff between iterations and look for what is changing.
- **Pagination:** when you exhaust a page and need to go to the next, that *is* a source-tab navigation. Treat each page as its own run with its own initial snapshot.

---

*Authored by [@ejaircastillo](https://github.com/ejaircastillo). Originally proposed in [#853](https://github.com/vercel-labs/agent-browser/issues/853) (March 2026), based on field work scraping geographic data for the Usina de Justicia crime map in Santa Cruz, Argentina. The `batch` command ([#865](https://github.com/vercel-labs/agent-browser/pull/865)) and stable tab ids ([#892](https://github.com/vercel-labs/agent-browser/pull/892)) cited in the reference implementation grew out of the same discussion thread and made the pattern significantly cleaner to express.*
