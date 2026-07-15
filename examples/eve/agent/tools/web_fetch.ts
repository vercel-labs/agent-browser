import { disableTool } from "eve/tools";

// Drop the web_fetch default for the same reason as web_search: all web
// content should flow through the agent-browser tools so pages render with a
// real browser (JavaScript, cookies, screenshots) inside the sandbox.
export default disableTool();
