---
name: slack
description: Interact with Slack workspaces using browser automation. Use when the user needs to check unread channels, navigate Slack, send messages, extract data, find information, search conversations, or automate any Slack task. Triggers include "check my Slack", "what channels have unreads", "send a message to", "search Slack for", "extract from Slack", "find who said", or any task requiring programmatic Slack interaction.
allowed-tools: Bash(agent-browser:*), Bash(npx agent-browser:*)
---

# Slack Automation

Interact with Slack workspaces to check messages, extract data, and automate common tasks.

## Quick Start

Connect to an existing Slack browser session or open Slack:

```bash
# Connect to existing session on port 9222 (typical for already-open Slack)
agent-browser connect 9222

# Or open Slack if not already running
agent-browser open https://app.slack.com
```

Then take a snapshot to see what's available:

```bash
agent-browser snapshot -i
```

## Core Workflow

1. **Connect/Navigate**: Open or connect to Slack
- **Snapshot**: Get interactive elements with refs (e.g. `@e1`, `@e2`)
3. **Navigate**: Click tabs, expand sections, or navigate to specific channels
4. **Extract/Interact**: Read data or perform actions
5. **Screenshot**: Capture evidence of findings

```bash
# Example: Check unread channels
agent-browser connect 9222
agent-browser snapshot -i
# Find the "More unreads" button by text
agent-browser find text "More unreads" click
agent-browser screenshot slack-unreads.png
```

## Common Tasks

### Checking Unread Messages

```bash
# Connect to Slack
agent-browser connect 9222

# Take snapshot to locate unreads button
agent-browser snapshot -i

# Look for:
# - "More unreads" button (usually near top of sidebar)
# - "Unreads" toggle in Activity tab (shows unread count)
# - Channel names with badges/bold text indicating unreads

# Navigate to Activity tab to see all unreads in one view
agent-browser snapshot -i
agent-browser find role tab click --name "Activity"
agent-browser wait 1000
agent-browser screenshot activity-unreads.png

# Or check DMs tab
agent-browser snapshot -i
agent-browser find role tab click --name "DMs"
agent-browser screenshot dms.png

# Or expand "More unreads" in sidebar
agent-browser snapshot -i
agent-browser find text "More unreads" click
agent-browser wait 500
agent-browser screenshot expanded-unreads.png
```

### Navigating to a Channel

```bash
# Search for channel in sidebar or by name
agent-browser snapshot -i

# Find channel name in the list (e.g., "engineering", "product-design")
# and click on it
agent-browser find text "engineering" click
agent-browser wait --load networkidle
agent-browser screenshot channel.png
```

### Finding Messages/Threads

```bash
# Use Slack search
agent-browser snapshot -i
agent-browser find role button click --name "Search"
agent-browser snapshot -i
agent-browser find role searchbox fill "keyword" --name "Search"
# Or if the search input has a placeholder:
# agent-browser find placeholder "Search" fill "keyword"
agent-browser press Enter
agent-browser wait --load networkidle
agent-browser screenshot search-results.png
```

### Extracting Channel Information

```bash
# Get list of all visible channels
agent-browser snapshot --json > slack-snapshot.json

# Parse for channel names and metadata
# Look for treeitem elements with level=2 (sub-channels under sections)
```

### Checking Channel Details

```bash
# Open a channel
agent-browser click @e_channel_ref
agent-browser wait 1000

# Get channel info (members, description, etc.)
agent-browser snapshot -i
agent-browser screenshot channel-details.png

# Scroll through messages
agent-browser scroll down 500
agent-browser screenshot channel-messages.png
```

### Taking Notes/Capturing State

When you need to document findings from Slack:

```bash
# Take annotated screenshot (shows element numbers)
agent-browser screenshot --annotate slack-state.png

# Take full-page screenshot
agent-browser screenshot --full slack-full.png

# Get current URL for reference
agent-browser get url

# Get page title
agent-browser get title
```

## Finding Elements Reliably

Slack assigns element refs (`@eN`) fresh on every snapshot — they are never stable across sessions. Always take a fresh snapshot and locate the element you need by name, role, or text before interacting with it. Do not rely on specific `@eN` numbers from examples or prior sessions.

**How to locate common elements:**

```bash
# Find the Search button
agent-browser snapshot -i
agent-browser find role button --name "Search"

# Find a tab by name (e.g., DMs, Activity)
agent-browser snapshot -i
agent-browser find role tab --name "DMs"

# Find "More unreads" button
agent-browser snapshot -i
agent-browser find text "More unreads"

# Find a specific channel in the sidebar by name
agent-browser snapshot -i
agent-browser find text "engineering" click
```

The `references/slack-tasks.md` file has detailed patterns for each task and a table of common ref discovery patterns organized by element type.

Understanding Slack's sidebar helps you navigate efficiently. The sidebar contains:

```
- Threads
- Huddles
- Drafts & sent
- Directories
- [Section Headers - External connections, Starred, Channels, etc.]
  - [Channels listed as treeitems]
- Direct Messages
  - [DMs listed]
- Apps
  - [App shortcuts]
- [More unreads] button (toggles unread channels list)
```

After clicking on a channel, you'll see tabs:
- **Messages** - Channel conversation
- **Files** - Shared files
- **Pins** - Pinned messages
- **Add canvas** - Collaborative canvas
- Other tabs depending on workspace setup

Click tab refs to switch views and get different information.

## Extracting Data from Slack

### Get Text Content

```bash
# Get a message or element's text
agent-browser get text @e_message_ref  # Replace with the actual message ref from a fresh snapshot
```

### Parse Accessibility Tree

```bash
# Full snapshot as JSON for programmatic parsing
agent-browser snapshot --json > output.json

# Look for:
# - Channel names (name field in treeitem)
# - Message content (in listitem/document elements)
# - User names (button elements with user info)
# - Timestamps (link elements with time info)
```

### Count Unreads

```bash
# After expanding the "More unreads" section, count the visible
# channel treeitems in the sidebar. This is a rough estimate:
# each treeitem with a channel name in the expanded unreads section
# corresponds to one channel with unread messages.
#
# For an accurate count, use the annotated screenshot of the
# expanded unreads section and count visually, or parse the
# JSON snapshot for treeitems within the unreads section only.
#
# Example: take a screenshot and count via visual inspection
agent-browser snapshot -i
agent-browser find text "More unreads" click
agent-browser wait 500
agent-browser screenshot unreads-count.png
# Then count the channel treeitems in the screenshot
```

## Best Practices

- **Connect to existing sessions**: Use `agent-browser connect 9222` if Slack is already open. This is faster than opening a new browser.
- **Take snapshots before clicking**: Always `snapshot -i` to identify refs before clicking buttons.
- **Re-snapshot after navigation**: After navigating to a new channel or section, take a fresh snapshot to find new refs.
- **Use JSON snapshots for parsing**: When you need to extract structured data, use `snapshot --json` for machine-readable output.
- **Pace interactions**: Add `sleep 1` between rapid interactions to let the UI update.
- **Check accessibility tree**: The accessibility tree shows what screen readers (and your automation) can see. If an element isn't in the snapshot, it may be hidden or require scrolling.
- **Scroll in sidebar**: Use `agent-browser scroll down 300 --selector ".p-sidebar"` to scroll within the Slack sidebar if channel list is long.

## Limitations

- **Cannot access Slack API**: This uses browser automation, not the Slack API. No OAuth, webhooks, or bot tokens needed.
- **Session-specific**: Screenshots and snapshots are tied to the current browser session.
- **Rate limiting**: Slack may rate-limit rapid interactions. Add delays between commands if needed.
- **Workspace-specific**: You interact with your own workspace -- no cross-workspace automation.

## Debugging

### Check console for errors

```bash
agent-browser console
agent-browser errors
```

### Get current page state

```bash
agent-browser get url
agent-browser get title
agent-browser screenshot page-state.png
```

## Example: Full Unread Check

```bash
#!/bin/bash

# Connect to Slack
agent-browser connect 9222

# Take initial snapshot
echo "=== Checking Slack unreads ==="
agent-browser snapshot -i > snapshot.txt

# Check Activity tab for unreads
agent-browser snapshot -i
agent-browser find role tab click --name "Activity"
agent-browser wait 1000
agent-browser screenshot activity.png
ACTIVITY_RESULT=$(agent-browser snapshot -i | grep -A1 "Activity" | tail -1)
echo "Activity: $ACTIVITY_RESULT"

# Check DMs
agent-browser snapshot -i
agent-browser find role tab click --name "DMs"
agent-browser wait 1000
agent-browser screenshot dms.png

# Check unread channels in sidebar
agent-browser snapshot -i
agent-browser find text "More unreads" click
agent-browser wait 500
agent-browser snapshot -i > unreads-expanded.txt
agent-browser screenshot unreads.png

# Summary
echo "=== Summary ==="
echo "See activity.png, dms.png, and unreads.png for full details"
```

## References

- **Slack docs**: https://slack.com/help
- **Web experience**: https://app.slack.com
- **Keyboard shortcuts**: Type `?` in Slack for shortcut list

### Reference Files

| Reference | Purpose |
|-----------|---------|
| [references/slack-tasks.md](references/slack-tasks.md) | Common Slack automation tasks and patterns (check unreads, find channels, search, monitor channels, extract conversations, track reactions, review pins) |
| [templates/slack-report-template.md](templates/slack-report-template.md) | Structured report template for Slack analysis findings |
