---
name: agent-browser
description: Browser automation using the local agent-browser daemon. Use this skill when the user wants to navigate websites, click elements, fill forms, take screenshots, scrape web pages, or automate online workflows.
---

# Agent-Browser Skill for Google Antigravity

This skill enables Antigravity to perform browser automation using the local `agent-browser` tool.

## Key Concepts

* **CDP-Driven Browser**: The tool launches and controls a headless or headful Chrome instance using the Chrome DevTools Protocol.
* **Accessibility Tree Refs**: Instead of relying solely on complex CSS/XPath selectors, `agent-browser` generates an accessibility tree where every interactive element has a unique reference number (e.g., `@e1`, `@e2`).
* **Tool Name Prefix**: All MCP tools are prefixed with `agent_browser_`.

## Workflow Guide

### 1. Opening a Website
Use `agent_browser_open` to launch the browser and navigate to a URL.
* Argument: `url` (e.g., `"https://instagram.com"`)

### 2. Getting the Page Structure
Always fetch a snapshot after loading a page or after performing an interaction to find the `@eN` references of the elements you want to target.
* Tool: `agent_browser_snapshot`

### 3. Interacting with Elements
Use the `@eN` references returned by the snapshot tool to interact:
* **Clicking**: `agent_browser_click` (arg: `selector` set to `"@e5"`)
* **Filling Input**: `agent_browser_fill` (args: `selector` set to `"@e3"`, `value` set to the text)
* **Typing keys**: `agent_browser_type` or `agent_browser_press` (e.g., to press Enter)

### 4. Waiting and Verifying
* **Wait**: `agent_browser_wait_ms` or `agent_browser_wait_for_selector`
* **Screenshots**: `agent_browser_screenshot` (saves a screenshot to verify what the page looks like)

## Common Troubleshooting
* **Elements covered**: If a click fails because another element is covering it, check the snapshot output to see if there is a cookie banner or dialog to dismiss first.
* **Dynamic content**: Wait a short amount of time using `agent_browser_wait_ms` or take another snapshot if the page is loading new content dynamically.
