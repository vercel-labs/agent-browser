# Events demo

The dashboard agent inspects a form, subscribes to its hidden completion element, submits, and ends its turn. The simulated upload validates for 2 seconds, uploads for 8, then builds for 4 before revealing `#upload-complete`. Use `?speed=2` for double speed. There is no real upload or event-bus integration in the page.

## Setup

Install Chrome and Python 3. Build the dashboard with `cd packages/dashboard && pnpm build`; debug binaries read `packages/dashboard/out/` on startup, while release binaries embed it at compile time. Use a binary with the `events` command:
```sh
export AB_BIN=/ABSOLUTE/PATH/cli/target/debug/agent-browser
examples/events-demo/run-demo.sh --live
```
`--live` reads the macOS keychain service `Vercel AI Gateway`, account `vercel-desktop-key`, without printing the key. Omit the flag to use an existing `AI_GATEWAY_API_KEY`. Ctrl-C stops the services the script started.

Use `examples/events-demo/run-demo.sh --fake` for scripted rehearsal without credentials (`AI_GATEWAY_URL=http://127.0.0.1:4950`, `AI_GATEWAY_API_KEY=fake`). Run the gateway separately with `python3 examples/events-demo/fake-gateway.py --port 4950`. It discovers refs and the completion selector from tool results, subscribes before clicking, ends its turn, then reads the subscription on wake.

For manual setup, run `examples/events-demo/serve.sh` in one terminal. In another, run `AI_GATEWAY_API_KEY="$(security find-generic-password -s 'Vercel AI Gateway' -a vercel-desktop-key -w)" || exit 1; export AI_GATEWAY_API_KEY`, then `"$AB_BIN" dashboard stop` and `"$AB_BIN" dashboard start`.

Open http://localhost:4848, select a session, and paste the script's printed prompt (replace the absolute path below if starting manually):
```text
Open http://127.0.0.1:4900, fill the deployment form with project name Acme Dashboard, environment Production, attach /ABSOLUTE/PATH/examples/events-demo/deployment-package.zip, and deploy it. Do not poll and do not block: subscribe to whatever tells you the upload has finished, submit, then stop. Tell me when the event wakes you.
```
Between takes, run `"$AB_BIN" events list`, then `"$AB_BIN" events stop <id>` for each subscription. Reload with `"$AB_BIN" open http://127.0.0.1:4900` or press “Deploy another”.

`deployment-package.zip` is a valid upload fixture containing only `README.txt` and `package.json`, not a bundle of the page or gateway; it needs no rebuild. See [Streaming](../../docs/content/docs/streaming.mdx) for event behavior, limits, architecture and harness setup recipes.
