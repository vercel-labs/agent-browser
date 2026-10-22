# Events comparison

Generated: 2026-10-22T04:59:59.495Z

- Binary: `/Users/mohamedlahfir/.worktrees/agent-browser-event-sub/wt/benchmarks/../cli/target/debug/agent-browser`
- Session: `events-compare-12625`
- Delay: 1500 ms
- Runs per pair: 3
- Signal cap: 10000 ms

| Scenario | Approach | Completed | Extra calls | Latency median (ms) | Daemon CPU median (ms) | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| element appears | events | 3/3 | 2.0 | 4.0 | 10.0 |  |
| element appears | wait-fn | 3/3 | 1.0 | 56.0 | 10.0 |  |
| element appears | eval-promise | 3/3 | 1.0 | 15.0 | 10.0 |  |
| element appears | polling | 3/3 | 6.0 | 12.0 | 10.0 |  |
| dialog opens | events | 3/3 | 2.0 | 2.0 | 0.0 |  |
| dialog opens | wait-fn | 3/3 | 1.0 | 56.0 | 10.0 |  |
| dialog opens | eval-promise | 3/3 | 1.0 | 21.0 | 10.0 |  |
| dialog opens | polling | 3/3 | 6.0 | 60.0 | 10.0 |  |
| navigation commits | events | 3/3 | 2.0 | 1.0 | 10.0 |  |
| navigation commits | wait-fn | 3/3 | 1.0 | 48.0 | 10.0 |  |
| navigation commits | eval-promise | 0/1 | - | - | - | CDP error (Runtime.evaluate): Inspected target navigated or closed |
| navigation commits | polling | 3/3 | 7.0 | 236.0 | 10.0 |  |

Extra agent calls counts CLI invocations after `open`. Events uses `events start` plus `events wait`. The signal timestamp read used to compute latency is measurement overhead and is not counted.

Latency is the same-host wall clock difference between the page writing `window.__signalAt` and the approach command returning. Daemon CPU is the `ps` cputime delta of the session daemon across the approach command.
