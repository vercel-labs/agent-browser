---
name: vercel
description: Access and test Vercel deployments with agent-browser, behind Deployment Protection or at a different feature flag variant. Use when a preview or production URL redirects to a Vercel login page, returns a protection 401 or 403, needs short-lived Trusted Sources OIDC authentication instead of a static bypass secret or public exception, or must be viewed with a Vercel feature flag set to another variant.
allowed-tools: Bash(agent-browser:*), Bash(npx agent-browser:*), Bash(vc:*), Bash(vercel:*), Bash(bunx:*)
---

# Vercel deployments

Reach a Vercel deployment as the user asked to see it without changing what anyone else sees. Pass protection and flag overrides on this browser's requests; changing project settings or a flag's served variant needs explicit approval.

## Deployment Protection

Use the caller's existing Vercel identity and a short-lived OIDC token. Do not disable Deployment Protection, make the deployment public, or ask for a static bypass secret first.

### Same-project preview

A local development token for the target project can access that project's protected Preview deployments through the default Trusted Sources self-access rule. No Trusted Sources configuration is normally required.

Confirm the local identity and Vercel CLI version:

```bash
vc whoami
vc --version
```

Require Vercel CLI `53.3.0` or newer before running `vc project token`. Versions `50.25.0` through `53.2.x` write the token to stderr, so command substitution captures nothing and the credential can appear in logs. If the installed version is older, stop and ask the user to upgrade it. Do not attempt to capture or recover the token from stderr.

Set the target project and scope explicitly. If they cannot be inferred safely, ask the user. In a directory whose existing `.vercel/project.json` link has been verified against the target, `vc project token` without a project name is also valid. Do not run `vc link` merely to get an OIDC token: current Vercel CLI versions also pull development variables into `.env.local` when linking.

Create a named browser session, mint a development OIDC token with the Vercel CLI, then inject it without printing or persisting it:

```bash
export AGENT_BROWSER_SESSION="$(agent-browser session id --scope worktree --prefix vercel-preview)"
export VERCEL_PREVIEW_URL="https://my-app.vercel.app"
export VERCEL_PROJECT="my-app"
export VERCEL_SCOPE="my-team"

(
  TOKEN="$(vc project token "$VERCEL_PROJECT" --scope "$VERCEL_SCOPE")"
  test -n "$TOKEN"
  agent-browser open "$VERCEL_PREVIEW_URL" --headers \
    "{\"x-vercel-trusted-oidc-idp-token\":\"$TOKEN\"}"
)

agent-browser snapshot -i
```

Continue the normal workflow in that same session. If your harness runs each command in a new shell, the exported `AGENT_BROWSER_SESSION` is gone afterwards, so pass the same name with `--session` on later commands. The header is scoped to the target origin and applies to the document, scripts, styles, fonts, and in-page requests. If the browser session is closed or restarted, repeat the authenticated `open` command.

Never print the token, paste it into source, or save it in an environment file.

### Other environments and callers

Trusted Sources configuration is needed when:

- a local development token must reach a protected Production deployment;
- the caller belongs to another Vercel project or team;
- the target project's self-access rules were customized; or
- Vercel returns `TRUSTED_SOURCES_ENVIRONMENT_MISMATCH`.

There is no supported Vercel CLI or public REST API for editing Trusted Sources rules. An authorized human must open the target project's **Settings → Deployment Protection → Trusted Sources** and add only the required caller and environment mapping. A local token has the `development` environment, so protected Production access requires `development` to `production`.

Stop and hand off the exact rule to the human. Do not use browser automation to change access control, and do not broaden unrelated environment mappings. Retry the authenticated `open` after the human confirms the rule is saved.

### Human intervention boundaries

The same-project development to Preview path should run without human intervention when the Vercel CLI is already authenticated and the target project and scope are known. A human is needed only when:

- the Vercel CLI has no authenticated identity and no existing `VERCEL_TOKEN`; interactive `vc login` requires the user;
- the installed Vercel CLI is older than `53.3.0` and must be upgraded before token minting;
- the correct target project or scope cannot be inferred safely for token minting;
- a Trusted Sources rule must be added or changed; the dashboard is the only supported management surface, and this changes access control;
- Secure Backend Access with OIDC Federation was disabled on the calling project and must be re-enabled in **Settings → Security**; or
- the static-secret fallback must be enabled or rotated and the agent needs explicit authorization for that access-control change. After approval, the agent can use `vc project protection` instead of requiring dashboard interaction.

The agent can diagnose each case and state the exact action required, then continue after the user confirms completion.

### Use the correct header

Send the Vercel-issued token as:

```text
x-vercel-trusted-oidc-idp-token: <VERCEL_OIDC_TOKEN>
```

Do not substitute `x-vercel-oidc-token`. That header carries workload identity into a Vercel Function; it does not authenticate an inbound request through Deployment Protection.

### Diagnose failures

- Redirect to `vercel.com/login`: Deployment Protection did not accept the request.
- `TRUSTED_SOURCES_ENVIRONMENT_MISMATCH`: the token is valid, but its caller environment cannot reach the target environment.
- Application `401` or `403` after protection passes: debug the application's own authentication separately.
- Application `404` on a deliberately missing route: the request passed Deployment Protection and reached the application.

### Static-secret fallback

Use Protection Bypass for Automation only when OIDC is not viable or the tool cannot send the Trusted Sources header. Enabling or rotating it changes access control, so obtain explicit authorization first. Create a dedicated secret so it can be rotated independently, keep it in an environment variable, and pass it as a header:

```bash
vc project protection enable <project> --protection-bypass \
  --protection-bypass-secret "$VERCEL_AUTOMATION_BYPASS_SECRET"

agent-browser open "$VERCEL_PREVIEW_URL" --headers \
  "{\"x-vercel-protection-bypass\":\"$VERCEL_AUTOMATION_BYPASS_SECRET\",\"x-vercel-set-bypass-cookie\":\"true\"}"
```

The cookie directive creates a reusable `_vercel_jwt` cookie. Treat saved browser state containing that cookie as a credential.

## Feature flags

To view a variant other than the one an environment serves, set an override cookie in this browser. Do not use `vc flags set`, `enable`, `disable`, `split`, or `rollout` to view a variant, because they change it for everyone using the environment.

Flags declared with `flag()` from the [Flags SDK](https://flags-sdk.dev/docs/api-reference/frameworks/next) apply overrides from the `vercel-flag-overrides` cookie. The cookie value is encrypted with the `FLAGS_SECRET` of the environment under test. Find the flag key and its variants in the source or with `vc flags inspect <flag>`, then mint the cookie without printing the secret. Pulling the secret with `--project` requires Vercel CLI `56.3.0` or newer; if the installed version is older, ask the user to upgrade it.

```bash
(
  set -e
  if [ -z "$FLAGS_SECRET" ]; then
    ENV_FILE="$(mktemp)"
    vc env pull "$ENV_FILE" --environment preview \
      --project "$VERCEL_PROJECT" --scope "$VERCEL_SCOPE" --yes >/dev/null
    FLAGS_SECRET="$(grep '^FLAGS_SECRET=' "$ENV_FILE" | cut -d= -f2- | tr -d '"')"
    rm "$ENV_FILE"
  fi
  case "$FLAGS_SECRET" in
    "" | "[SENSITIVE]") echo "FLAGS_SECRET is not readable; ask the user for it" >&2; exit 1 ;;
  esac
  OVERRIDE="$(FLAGS_SECRET="$FLAGS_SECRET" bunx --package flags bun -e \
    'import { encryptOverrides } from "flags"; console.log(await encryptOverrides({ "my-flag": "variant-b" }))')"
  test -n "$OVERRIDE"
  agent-browser cookies set vercel-flag-overrides "$OVERRIDE" --url "$VERCEL_PREVIEW_URL"
  agent-browser reload
  agent-browser screenshot
)
```

Run this in the same session as the protected `open`, and confirm in the screenshot that the requested variant rendered before reporting. `agent-browser cookies clear` removes the override along with every other cookie, so repeat the protected `open` after clearing.

If `FLAGS_SECRET` is stored as a Secret, `vc env pull` writes the placeholder `[SENSITIVE]` instead of the value because Secret values are write-only, and the script stops. Ask the user to export `FLAGS_SECRET` in your environment and run it again; the script uses an exported value instead of pulling one.

If the app evaluates flags without the Flags SDK or through another provider, the cookie has no effect and there is no request-scoped path. Ask the user before changing the flag with `vc flags set`, record the original variant, restore it afterwards, and verify the restore with `vc flags inspect`.

## Avoid dead ends

- There is no `vercel share` CLI command. Shareable Links are intended for people and are not the automation path.
- `vercel curl` is useful for HTTP requests, but it cannot render and interact with a page.
- Changing a flag's served variant to take a screenshot changes it for teammates, CI, and users of that environment. Use the override cookie.
- Deployment Protection Exceptions make the domain public. Do not use them merely to unblock an agent.
- Do not expose OIDC tokens, bypass secrets, authenticated URLs, or saved state in logs, screenshots, source files, or user-facing output.
