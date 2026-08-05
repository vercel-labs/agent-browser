export const ALLOWED_URLS = [
  "https://example.com",
  "https://ai-sdk.dev",
  "https://useworkflow.dev",
  "https://vercel.com",
] as const;

export type AllowedUrl = (typeof ALLOWED_URLS)[number];
