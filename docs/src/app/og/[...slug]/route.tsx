import { NextResponse } from "next/server";
import { getPageTitle, renderOgImage } from "../og-image";

export async function GET(
  _request: Request,
  { params }: { params: Promise<{ slug: string[] }> },
) {
  const { slug } = await params;
  const title = getPageTitle(slug.join("/"));

  if (!title) {
    return NextResponse.json({ error: "Not found" }, { status: 404 });
  }

  return renderOgImage(title);
}
