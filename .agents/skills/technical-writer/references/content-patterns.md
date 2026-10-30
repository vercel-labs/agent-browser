# Content patterns

Select the pattern that serves the reader's task. These are planning aids, not mandatory section lists. Follow the destination's established architecture when it already works.

## Task guide

Open with the outcome and any constraint that determines whether the guide applies. Put prerequisites before the first dependent action. Use numbered steps with imperative headings, exact file locations or UI labels, and relevant verification points. Finish with the observable result and any necessary cleanup or next step.

Explain why a step is needed when that knowledge helps readers choose correctly or avoid a likely failure. Do not narrate every obvious click. Place troubleshooting beside the relevant step when possible; use a separate section for failures that span the procedure.

## Tutorial

Define what the reader will build and what they will learn by building it. State the assumed knowledge and starting environment. Develop one coherent example through the steps. Explain the mechanism behind consequential choices rather than presenting an unexplained sequence of commands.

Include checkpoints so readers can locate a mistake before it compounds. Distinguish teaching shortcuts from production requirements. End with a working result and a relevant extension or reference when useful.

## Conceptual explanation

Answer the central question first, then explain the mechanism, the conditions in which it applies, and the tradeoffs. Use an example to make the explanation concrete. A diagram can help when relationships or lifecycle stages are difficult to describe in prose; keep labels aligned with the terms used in the text.

Explain the concept using the SDK's actual behavior and terminology. Distinguish related concepts that developers might confuse, and link to the relevant API reference or implementation guide. Avoid abstractions that never connect to code the reader will use.

## API or configuration reference

Use exact symbols and names as titles when readers are looking up an interface. Explain purpose, signature, parameters, types, required fields, defaults, return values, errors, and side effects where relevant. Distinguish an omitted value from `null`, an empty value, and a disabled setting.

Use consistent tables for repeated attributes. Include version restrictions and a minimal valid example. Link to a conceptual explanation or task guide instead of embedding a full tutorial in every entry. Keep descriptions precise enough to distinguish similar options.

## Troubleshooting

Start with the symptom and affected context. Quote exact errors only when verified. Provide a diagnostic sequence that separates likely causes, then explain each relevant fix and how to confirm recovery.

Avoid a generic list of resets or retries. Explain what each check establishes. If a step can delete data, reset configuration, or affect a live service, state the consequence before the step. Give an escalation path with the diagnostic information to collect, excluding credentials and other secrets.

## Migration guide

State the source and destination scope, supported versions, and what the migration changes. Identify prerequisites and map concepts only where the mapping is valid. Explain differences that require redesign instead of suggesting equivalence.

Cover state or data movement, configuration, dependencies, validation, cutover, and rollback where relevant. Identify interruption or compatibility risks at the affected step. Do not imply a code migration transfers data, domains, or credentials automatically.

For SDK version migrations, show before-and-after code for changed APIs. Explain changes to imports, types, defaults, and runtime behavior. Identify what an available codemod handles and what requires manual review, based on verified behavior.

## Integration guide

State which SDK, adapter, provider, or framework the guide connects and which versions are compatible. Explain the required setup on each side, then show a complete path from input to observable result.

Identify which layer owns configuration, authentication, state, and errors. Document meaningful capability differences and unsupported combinations. Link to the dependency's reference for details it owns while keeping enough context to complete the integration.

## Getting started

Lead the reader from the stated starting environment to one working result. Include installation, required configuration, a minimal complete example, and a way to verify it works. Explain where each file belongs.

Choose one coherent path instead of introducing every API and customization point. Link to further guides at the moment they become useful. Include necessary security and execution boundaries even in the smallest example.

## Cookbook example

Solve one concrete task with a focused, complete example. State prerequisites and the relevant environment, explain non-obvious choices, and describe the expected result. Link to the API reference for detailed option semantics.

Keep the example consistent with the guide and reference for the same API. If it demonstrates a variation, make the difference explicit rather than silently changing assumptions.

## Release note or changelog

State what changed, who is affected, availability, and any action required. Include compatibility or migration details when they matter. Distinguish a fix from a new capability and a preview from general availability. Avoid promotional claims that obscure the actual change.
