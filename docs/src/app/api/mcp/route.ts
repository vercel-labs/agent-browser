import { createMcpRoute } from "@vercel/geistdocs/routes/mcp";
import { config } from "@/lib/geistdocs/config";
import { GET as search } from "../search/route";

const handler = createMcpRoute({ config, search });

export { handler as GET, handler as POST };
