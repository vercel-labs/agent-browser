# Events demo: presenter guide

This demo shows the dashboard chat agent submitting a slow upload form without polling or blocking. The agent reads the page markup, subscribes to the element that will appear when the upload finishes, submits, and ends its turn. When the subscription fires, the dashboard delivers the event to the chat as a new message and the agent resumes on its own to report the outcome.

Nothing in the form page is wired to the events feature. `index.html` is an ordinary web form, and the agent has to work out which element signals completion from the markup alone.

## Files

- `index.html`: the form page. It runs a staged fake upload of about 14 seconds (validating, uploading with a progress bar, building) and then reveals a hidden `#upload-complete` panel. Add `?speed=2` to the URL to run twice as fast, or `?speed=0.5` to run at half speed.
- `deployment-package.zip`: a tiny valid zip to attach in the form.
- `serve.sh`: serves this folder at http://127.0.0.1:4900.
- `run-demo.sh`: starts the page server and the dashboard in one terminal and cleans up on exit.
- `fake-gateway.py`: an OpenAI compatible test double for rehearsal and fallback. It is not the demo path.

## Prerequisites

- A binary built with `cargo build --features event-prototype` from the `cli/` folder.
- Google Chrome installed.
- The dashboard built so the Events tab exists. Run `pnpm build` in `packages/dashboard`, then `cargo build --features event-prototype` from `cli/`. A debug binary reads `packages/dashboard/out/` from disk when it starts, so restarting the dashboard is enough after a later UI rebuild. A release build embeds `out/` at compile time. Run the feature build last: a plain `cargo build` or `cargo test` without the feature replaces the binary with one that has no `events` command, and the agent's `events start` will then be refused.
- A Vercel AI Gateway key in the macOS keychain (service `Vercel AI Gateway`, account `vercel-desktop-key`), or use the fake gateway described below.
- `python3` on the path.

Point the scripts at your build:

```bash
export AB_BIN=/ABSOLUTE/PATH/cli/target/debug/agent-browser
```

## Start the demo

The one terminal way:

```bash
examples/events-demo/run-demo.sh --live
```

The script reads the key from the keychain without printing it, starts the page server and the dashboard, prints the URLs and the prompt to paste, and stops everything it started when you press Ctrl-C. Run it without a flag to use whatever `AI_GATEWAY_API_KEY` is already in your environment.

The same thing by hand in three terminals:

Terminal 1, the page:

```bash
examples/events-demo/serve.sh
```

Terminal 2, the dashboard with the gateway key (the key is never printed):

```bash
AI_GATEWAY_API_KEY="$(security find-generic-password -s 'Vercel AI Gateway' -a vercel-desktop-key -w)" || exit 1; export AI_GATEWAY_API_KEY
"$AB_BIN" dashboard stop
"$AB_BIN" dashboard start
```

Terminal 3, optional, to reset between takes:

```bash
"$AB_BIN" events list
"$AB_BIN" events stop <id>
"$AB_BIN" open http://127.0.0.1:4900
```

Open http://localhost:4848 and select the session you want the chat to drive.

## The prompt

Paste this into the dashboard chat, replacing the path with the real absolute path of `deployment-package.zip` (`run-demo.sh` prints the line with the path filled in):

```text
Open http://127.0.0.1:4900, fill the deployment form with project name Acme Dashboard, environment Production, attach /ABSOLUTE/PATH/examples/events-demo/deployment-package.zip, and deploy it. Do not poll and do not block: subscribe to whatever tells you the upload has finished, submit, then stop. Tell me when the event wakes you.
```

## Talk track

Times are approximate and depend on model latency. With a live model the whole run is about one to two minutes.

- 0:00 to 0:10. Paste the prompt. Say that the agent has one tool, a single agent-browser command per call, and that you have told it neither to poll nor to block.
- 0:10 to 0:40. The agent opens the page and inspects the markup. The audience sees the form fill in on the viewport: project name, environment, then the package file name and size in the dropzone. Point out that the agent is looking for something hidden in the page that will show up when the work is done.
- 0:40 to 0:55. The agent starts an events subscription for the element it picked, most likely `#upload-complete`. Open the Events tab and show the subscription waiting. This is the moment to say that nothing in the page told it to do this.
- 0:55 to 1:05. The agent clicks Deploy and then ends its turn with one short sentence saying what it is waiting for. The chat goes idle: the agent is not running, not polling, and not blocked inside a tool call. The Events row keeps pulsing while the progress card runs through Validating package, Uploading with a percentage, and Building.
- About 1:10 to 1:25. The completion panel becomes visible and the Events row turns to the fired state. Moments later an "Event received" card appears in the chat with the selector and what happened. That card is the dashboard delivering the event to the agent as a new message.
- Right after. The agent resumes on its own, without anyone typing, and tells you in the chat that the upload finished. Close by saying the same pattern works for any element that appears, with no loop in the agent and nothing specific to forms.

## Fallback

If the network or the key fails, use the fake gateway. It replays a scripted agent run through the same tool, using only public commands, so the dashboard shows the same flow:

```bash
examples/events-demo/run-demo.sh --fake
```

This starts `fake-gateway.py` on 127.0.0.1:4950 and sets `AI_GATEWAY_URL=http://127.0.0.1:4950` and `AI_GATEWAY_API_KEY=fake` for the dashboard. The fake agent reads the URL, the file path, and the project name from your chat message, so use the same prompt. It ends its first turn after the click and answers the event message that the dashboard sends when the subscription fires. It also works on its own with `python3 examples/events-demo/fake-gateway.py --port 4950`.

To reset between takes, run `events stop` for each id shown by `events list`, then reload the page with `open http://127.0.0.1:4900` or press "Deploy another" in the form.

## Limits

- Chrome only. The events feature is not available on the other engines.
- It watches the current document only. It does not follow the agent across tabs or frames.
- The agent has to infer the signal from the page markup. A page with no usable completion element gives it nothing to subscribe to, and a weaker model may pick the wrong element or fall back to polling.
- The wake-up comes from the dashboard chat panel, so the dashboard has to stay open (the chat can be on another tab) and the chat has to be idle when the event fires. If the agent is mid-turn, the message is sent when the turn ends.
- A page that navigates on submit would end the watch, because the document the subscription lives in is replaced.
- The form's progress is simulated in the browser. There is no server and no real upload.
