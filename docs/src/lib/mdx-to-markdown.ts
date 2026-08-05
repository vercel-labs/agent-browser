/**
 * Converts raw MDX content to clean Markdown suitable for AI agents.
 *
 * Strips export/import statements and standalone JSX divs with className
 * attributes, passing everything else through as valid Markdown.
 */
export function mdxToCleanMarkdown(raw: string): string {
  const lines = raw.split("\n");
  const out: string[] = [];
  let inJsxBlock = false;
  let jsxDepth = 0;

  for (const line of lines) {
    const trimmed = line.trim();

    if (trimmed.startsWith("export ") || trimmed.startsWith("import ")) {
      continue;
    }

    if (
      !inJsxBlock &&
      trimmed.startsWith("<div ") &&
      trimmed.includes("className=")
    ) {
      inJsxBlock = true;
      jsxDepth = 1;
      continue;
    }

    if (inJsxBlock) {
      const opens = (line.match(/<div[\s>]/g) || []).length;
      const closes = (line.match(/<\/div>/g) || []).length;
      jsxDepth += opens - closes;
      if (jsxDepth <= 0) {
        inJsxBlock = false;
        jsxDepth = 0;
      }
      continue;
    }

    out.push(line);
  }

  let result = out.join("\n");
  result = result.replace(/^\n+/, "\n").trim();
  return result;
}
