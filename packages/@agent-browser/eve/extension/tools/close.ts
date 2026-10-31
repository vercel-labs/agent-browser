import { catalogTool } from "../lib/catalog";

// `all` would close every session in the sandbox, not just this extension's.
export default catalogTool("close", { hiddenArguments: ["all"] });
