# Codegen

`agent-browser codegen` turns supported successful browser actions into a reusable Chrome DevTools Recorder flow. Start capture before performing actions, then stop it to print or save the result.

```bash
agent-browser codegen start --title "login flow"
agent-browser open https://example.com/login
agent-browser fill "#email" "a@example.com"
agent-browser click "#submit"
agent-browser isvisible "#welcome"
agent-browser codegen stop ./login.flow.json
```

The default output is JSON for Chrome DevTools Recorder and `@puppeteer/replay`. To write a Playwright test instead, use `agent-browser codegen stop ./login.spec.ts --format playwright`.

Direct CSS selectors, `xpath=` selectors, snapshot refs with accessible names, and uniquely probed test IDs can become safe targets. Codegen omits an action when it cannot produce a safe target. Direct `text=` selectors, bare XPath, semantic locator marker actions, most wait variants, `evaluate`, and `webmcp invoke` are not recorded. `snapshot`, screenshots, page reads, `webmcp list`, `webmcp result`, and other observation commands do not become flow steps. Other successful mutating or wait commands produce omission warnings.

`record start --url` navigates the active page, so codegen records that move as a normal navigation step. `record start` without a URL and `record stop` change no page and record nothing. If another command that codegen cannot record moves a page, codegen reports an `unrecorded-navigation` warning. The flow then has no step that reaches the new page, so the next `navigate` on that page always emits a step, even when the URL looks unchanged.

A new tab inherits the session setup, which can include init scripts and authentication state. A generated artifact does not contain that setup. Add it to the test yourself when the flow needs it.

The command captures typed values verbatim, including password values, selected values, and upload paths. The daemon restores an unfinished flow from its owner-only append-only journal after a restart. `agent-browser codegen status` reports active, restored, degraded, recovery-error, and cleanup-pending states. It also reports the journal path, capture and security warnings, and projected Recorder and Playwright counts. Use `agent-browser codegen discard` to delete an unfinished or damaged flow. A successful stop removes the journal. Treat the journal and generated artifact as sensitive material and do not commit credentials.

Codegen prefers a verified target: a unique test ID, a unique probed CSS selector, or the exact role and accessible name from a snapshot. When only the selector you typed is available, the Playwright spec uses `.first()`, which matches what the command did, and reports `playwright-target-not-unique`, and Recorder reports `recorder-target-not-unique` for the same target. A count assertion keeps every match.

A `check` or `uncheck` command that finds the control already in the requested state changes nothing. Codegen records an assertion for that state instead of a click, because a replayed click would clear the control. A scroll is recorded as the position the page reached, so repeated scrolls replay to the same place.

Recorder JSON omits sequential typing, multi-value select, upload, key chords, and explicit page creation because its schema cannot keep the exact action intent. It converts back, forward, and reload to navigation to the observed final URL and reports a lossy warning. Recorder identifies a non-main page by URL, so same-URL pages are ambiguous. Playwright keeps typed selector kinds, multi-value select, upload paths, typing clear mode and delay, modifier chords, pointer input, element and page scroll, logical page identity, scoped close, and initial mobile context options. `codegen stop` reports captured action, internal step, emitted step, omitted step, and lossy step counts. It also reports capture, security, and cleanup warning counts. Grouped format warnings include stable codes and affected action IDs.
