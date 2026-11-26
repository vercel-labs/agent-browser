import { navigation } from "./docs-navigation";
import { loadAllDocsSources } from "./docs-source";

export type IndexEntry = {
  title: string;
  href: string;
  section: string;
  content: string;
};

let cached: IndexEntry[] | null = null;

function stripMarkdown(md: string): string {
  return md
    .replace(/```[\s\S]*?```/g, "")
    .replace(/`[^`]+`/g, "")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/\*{1,3}([^*]+)\*{1,3}/g, "$1")
    .replace(/<[^>]+>/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export async function getSearchIndex(): Promise<IndexEntry[]> {
  if (cached) return cached;
  const sources = new Map(
    (await loadAllDocsSources()).map((source) => [source.href, source]),
  );
  const entries = navigation.flatMap((section) =>
    section.items.map((item) => {
      const source = sources.get(item.href);
      if (!source) throw new Error(`Missing search source for ${item.href}`);
      return {
        title: item.name,
        href: item.href,
        section: section.title ?? "",
        content: stripMarkdown(source.legacyMarkdown),
      };
    }),
  );
  cached = entries;
  return entries;
}
