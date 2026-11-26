type MarkdownNode = {
  type: string;
  depth?: number;
  value?: string;
  children?: MarkdownNode[];
  data?: { hProperties?: Record<string, unknown> };
};

export function legacyHeadingId(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-")
    .trim();
}

function text(node: MarkdownNode): string {
  if (node.type === "image" || node.type === "mdxTextExpression") return "";
  return node.children ? node.children.map(text).join("") : (node.value ?? "");
}

export function remarkLegacyHeadings() {
  return function walk(node: MarkdownNode) {
    if (node.type === "heading" && node.depth && node.depth <= 3) {
      node.data ??= {};
      node.data.hProperties ??= {};
      node.data.hProperties.id = legacyHeadingId(text(node));
    }
    node.children?.forEach(walk);
  };
}
