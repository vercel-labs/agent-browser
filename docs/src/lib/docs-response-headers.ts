import { isPreviewDeployment } from "./site";

const negotiationHeaders = [
  "Accept",
  "User-Agent",
  "Signature-Agent",
  "Sec-Fetch-Mode",
  "Sec-Fetch-Dest",
  "RSC",
  "Next-Router-State-Tree",
  "Next-Router-Prefetch",
  "Next-Router-Segment-Prefetch",
  "Next-Url",
  "Purpose",
  "Sec-Purpose",
];

export function applyDocsResponseHeaders(headers: Headers) {
  const tokens = new Map<string, string>();
  for (const token of [
    ...(headers.get("Vary") ?? "").split(/\s*,\s*/),
    ...negotiationHeaders,
  ]) {
    if (token) tokens.set(token.toLowerCase(), token);
  }
  headers.set("Vary", tokens.has("*") ? "*" : [...tokens.values()].join(", "));
  headers.set("Cache-Control", "private, no-store");
  headers.set("CDN-Cache-Control", "no-store");
  headers.set("Vercel-CDN-Cache-Control", "no-store");
  if (isPreviewDeployment()) headers.set("X-Robots-Tag", "noindex, nofollow");
}
