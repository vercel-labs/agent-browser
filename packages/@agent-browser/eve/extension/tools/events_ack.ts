import { catalogTool } from "../lib/catalog";

export default catalogTool("events_ack", {
  injectOwner: true,
  forceContentBoundaries: true,
});
