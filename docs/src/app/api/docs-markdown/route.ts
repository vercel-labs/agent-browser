import { readFile } from "fs/promises";
import { join } from "path";
import { NextRequest, NextResponse } from "next/server";
import { mdxToCleanMarkdown } from "@/lib/mdx-to-markdown";

export async function GET(req: NextRequest) {
  const { searchParams } = new URL(req.url);
  const docPath = searchParams.get("path");

  if (!docPath) {
    return NextResponse.json(
      { error: "Missing ?path= parameter" },
      { status: 400 },
    );
  }

  const normalized = docPath
    .replace(/^\//, "")
    .replace(/\.\./g, "")
    .replace(/[^a-zA-Z0-9/_-]/g, "");

  const slug = normalized;
  const filePath = slug
    ? join(process.cwd(), "src", "app", ...slug.split("/"), "page.mdx")
    : join(process.cwd(), "src", "app", "page.mdx");

  try {
    const raw = await readFile(filePath, "utf-8");
    const markdown = mdxToCleanMarkdown(raw);

    return new NextResponse(markdown, {
      headers: {
        "Content-Type": "text/markdown; charset=utf-8",
        "Cache-Control": "public, max-age=3600",
      },
    });
  } catch {
    return NextResponse.json({ error: "Page not found" }, { status: 404 });
  }
}
