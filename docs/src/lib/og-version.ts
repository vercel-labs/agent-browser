import { createHash } from "node:crypto";

// Bump when the card layout or fonts change so social caches fetch a new image.
export const ogGeneratorVersion = "1";

export function versionedOgImage(path: string, title: string) {
  const version = createHash("sha256")
    .update(`${ogGeneratorVersion}:${title}`)
    .digest("hex")
    .slice(0, 8);
  return `${path}?v=${version}`;
}
