import { disableTool } from "eve/tools";

// Drop the provider-managed web_search default so the agent researches the
// web through its agent-browser tools (browser__open, browser__read, ...)
// instead of a search API.
export default disableTool();
