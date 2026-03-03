import { getPageTitle, renderOgImage } from "./og-image";

export async function GET() {
  const title = getPageTitle("")!;
  return renderOgImage(title);
}
