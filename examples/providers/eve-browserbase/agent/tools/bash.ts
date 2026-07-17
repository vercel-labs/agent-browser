import { disableTool } from "eve/tools";

// The Browserbase credential is present in the Hobby-compatible sandbox
// environment. Keep arbitrary shell commands out of the model's tool set so it
// is reachable only through the trusted @agent-browser/eve command runner.
export default disableTool();
