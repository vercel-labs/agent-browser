import { catalogTool } from "../lib/catalog";

// Outside the `core` profile: found with eve__search instead of listed every turn.
export default catalogTool("get_count", { deferred: true });
