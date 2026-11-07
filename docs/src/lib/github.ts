const REPO = "vercel-labs/agent-browser";
const REVALIDATE = 86400;

export async function getStarCount(): Promise<string> {
  try {
    const res = await fetch(`https://api.github.com/repos/${REPO}`, {
      headers: { Accept: "application/vnd.github.v3+json" },
      next: { revalidate: REVALIDATE },
    });
    if (!res.ok) return "";
    const data = await res.json();
    const count = data.stargazers_count;
    if (typeof count !== "number") return "";
    if (count >= 1000)
      return `${(count / 1000).toFixed(count >= 10000 ? 0 : 1)}k`;
    return String(count);
  } catch {
    return "";
  }
}
