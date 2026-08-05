import { NextRequest, NextResponse } from "next/server";
import { getSearchIndex } from "@/lib/search-index";

export async function GET(req: NextRequest) {
  const q = req.nextUrl.searchParams.get("q")?.trim().toLowerCase();

  if (!q) {
    return NextResponse.json({ results: [] });
  }

  const index = await getSearchIndex();
  const terms = q.split(/\s+/).filter(Boolean);

  const results = index
    .map((entry) => {
      const titleLower = entry.title.toLowerCase();
      const contentLower = entry.content.toLowerCase();

      const titleMatch = terms.every((t) => titleLower.includes(t));
      const contentMatch = terms.every((t) => contentLower.includes(t));

      if (!titleMatch && !contentMatch) return null;

      let snippet = "";
      if (contentMatch) {
        const firstTermIdx = Math.min(
          ...terms.map((t) => {
            const idx = contentLower.indexOf(t);
            return idx === -1 ? Infinity : idx;
          }),
        );
        if (firstTermIdx !== Infinity) {
          const start = Math.max(0, firstTermIdx - 40);
          const end = Math.min(entry.content.length, firstTermIdx + 120);
          snippet =
            (start > 0 ? "..." : "") +
            entry.content.slice(start, end).replace(/\n/g, " ") +
            (end < entry.content.length ? "..." : "");
        }
      }

      return {
        title: entry.title,
        href: entry.href,
        section: entry.section,
        snippet,
        score: titleMatch ? 2 : 1,
      };
    })
    .filter(
      (
        r,
      ): r is {
        title: string;
        href: string;
        section: string;
        snippet: string;
        score: number;
      } => r !== null,
    )
    .sort((a, b) => b.score - a.score)
    .slice(0, 20)
    .map(({ score: _, ...rest }) => rest);

  return NextResponse.json(
    { results },
    { headers: { "Cache-Control": "public, max-age=60" } },
  );
}
