"use client";

import { useMemo } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";

type AgentViewProps = {
  snapshot: string;
  connected: boolean;
};

type SnapshotNode = {
  role: string;
  name?: string;
  value?: string;
  kind?: string;
  hints: string[];
  attrs: Record<string, string>;
  children: SnapshotNode[];
};

const META_ROLES = new Set(["/url", "/placeholder"]);
const TEXT_ROLES = new Set(["statictext", "text", "paragraph"]);

export function AgentView({ snapshot, connected }: AgentViewProps) {
  const tree = useMemo(() => normalizeNodes(parseSnapshotTree(snapshot)), [snapshot]);
  const nodeCount = useMemo(() => countNodes(tree), [tree]);

  if (!connected) {
    return <EmptyState text="No browser connected" />;
  }

  if (!snapshot.trim()) {
    return <EmptyState text="Loading Agent view snapshot..." />;
  }

  if (snapshot.trim() === "(no interactive elements)") {
    return <EmptyState text="No interactive elements in current snapshot" />;
  }

  return (
    <ScrollArea className="h-full w-full">
      <div className="min-h-full bg-muted/20 px-4 py-3">
        <div className="mx-auto w-full max-w-6xl space-y-3">
          {tree.length > 0 ? (
            <div className="space-y-3 rounded-xl border border-border bg-background p-4">
              {tree.map((node, i) => (
                <AgentNodeView key={`${node.role}-${i}`} node={node} depth={0} />
              ))}
            </div>
          ) : (
            <pre className="rounded-xl border border-border bg-background p-3 font-mono text-xs whitespace-pre-wrap">
              {snapshot}
            </pre>
          )}
        </div>
      </div>
    </ScrollArea>
  );
}

function EmptyState({ text }: { text: string }) {
  return <div className="text-center text-sm text-muted-foreground">{text}</div>;
}

function AgentNodeView({ node, depth }: { node: SnapshotNode; depth: number }) {
  if (META_ROLES.has(node.role)) return null;

  const role = node.role.toLowerCase();
  const label = getLabel(node);
  const refId = node.attrs.ref;
  const children = dedupeChildren(
    node.children.filter((child) => !META_ROLES.has(child.role)),
    label,
  );

  if (TEXT_ROLES.has(role)) {
    const text = label || node.value || "";
    if (!text) return renderChildren(children, depth + 1);
    return <p className="text-sm leading-6 text-foreground/90">{text}</p>;
  }

  if (role === "heading") {
    const level = Math.max(1, Math.min(6, Number(node.attrs.level ?? "2")));
    return (
      <div className="space-y-1">
        <div className="flex flex-wrap items-center gap-2">
          {renderHeading(level, label || "Heading")}
          <NodeMeta refId={refId} kind={node.kind} />
        </div>
        {renderChildren(children, depth + 1)}
      </div>
    );
  }

  if (role === "button") {
    return (
      <div className="inline-flex items-center gap-2">
        <Button size="xs" variant="outline" disabled>
          {label || "Button"}
        </Button>
        <NodeMeta refId={refId} kind={node.kind} />
      </div>
    );
  }

  if (role === "link") {
    const href = node.children.find((child) => child.role === "/url")?.value ?? "#";
    return (
      <div className="inline-flex items-center gap-2">
        <a
          href={href}
          onClick={(e) => e.preventDefault()}
          className="text-sm text-primary underline underline-offset-4 hover:text-primary/80"
        >
          {label || href}
        </a>
        <NodeMeta refId={refId} kind={node.kind} />
      </div>
    );
  }

  if (role === "textbox" || role === "searchbox") {
    const value = node.value ?? "";
    const placeholder = node.children.find((child) => child.role === "/placeholder")?.value;
    return (
      <div className="flex max-w-xl items-center gap-2">
        <input
          readOnly
          value={value}
          placeholder={value ? undefined : placeholder || label || "Type here"}
          className="h-8 w-full rounded-md border border-input bg-background px-2 text-sm text-foreground"
        />
        <NodeMeta refId={refId} kind={node.kind} />
      </div>
    );
  }

  if (role === "checkbox" || role === "radio") {
    const checked = (node.attrs.checked ?? "").toLowerCase() === "true";
    return (
      <label className="inline-flex items-center gap-2 text-sm text-foreground">
        <input type={role === "radio" ? "radio" : "checkbox"} checked={checked} readOnly disabled />
        <span>{label || node.role}</span>
        <NodeMeta refId={refId} kind={node.kind} />
      </label>
    );
  }

  if (role === "combobox") {
    const options = children.filter((child) => child.role.toLowerCase() === "option");
    return (
      <div className="flex max-w-xl items-center gap-2">
        <select disabled className="h-8 w-full rounded-md border border-input bg-background px-2 text-sm text-foreground">
          {options.length > 0 ? (
            options.map((opt, i) => (
              <option key={`${opt.role}-${i}`}>{getLabel(opt) || opt.value || "Option"}</option>
            ))
          ) : (
            <option>{label || "Select option"}</option>
          )}
        </select>
        <NodeMeta refId={refId} kind={node.kind} />
      </div>
    );
  }

  if (role === "list") {
    return <ul className="list-disc space-y-1.5 pl-5">{renderChildren(children, depth + 1)}</ul>;
  }

  if (role === "listitem") {
    return (
      <li className="space-y-1">
        {label && <span className="text-sm text-foreground/90">{label}</span>}
        {renderChildren(children, depth + 1)}
      </li>
    );
  }

  if (role === "generic" && !label && !node.value && children.length === 1) {
    return <AgentNodeView node={children[0]} depth={depth + 1} />;
  }

  const containerClass = cn(
    "space-y-2",
    depth === 0 && "space-y-3",
    (role === "banner" || role === "navigation" || role === "section" || role === "region" || role === "contentinfo") &&
    "rounded-lg border border-border bg-card p-3",
    role === "generic" && depth > 0 && "border-l border-border/60 pl-3",
  );

  return (
    <div className={containerClass}>
      {label && role !== "document" && role !== "rootwebarea" && role !== "webarea" && role !== "main" && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-medium text-foreground">{label}</span>
          <NodeMeta refId={refId} kind={node.kind} />
        </div>
      )}
      {node.value && role !== "textbox" && role !== "searchbox" && (
        <p className="text-sm text-muted-foreground">{node.value}</p>
      )}
      {renderChildren(children, depth + 1)}
    </div>
  );
}

function renderChildren(children: SnapshotNode[], depth: number) {
  return children.map((child, i) => (
    <AgentNodeView key={`${child.role}-${i}`} node={child} depth={depth} />
  ));
}

function NodeMeta({ refId, kind }: { refId?: string; kind?: string }) {
  if (!refId && !kind) return null;
  return (
    <span className="inline-flex items-center gap-1">
      {refId && <Badge variant="outline" className="h-4 px-1 text-[10px] font-mono">@{refId}</Badge>}
      {kind && <Badge variant="secondary" className="h-4 px-1 text-[10px]">{kind}</Badge>}
    </span>
  );
}

function renderHeading(level: number, text: string) {
  if (level <= 1) return <h1 className="text-3xl font-semibold tracking-tight">{text}</h1>;
  if (level === 2) return <h2 className="text-2xl font-semibold tracking-tight">{text}</h2>;
  if (level === 3) return <h3 className="text-xl font-semibold tracking-tight">{text}</h3>;
  if (level === 4) return <h4 className="text-lg font-semibold">{text}</h4>;
  if (level === 5) return <h5 className="text-base font-semibold">{text}</h5>;
  return <h6 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">{text}</h6>;
}

function getLabel(node: SnapshotNode): string {
  return (node.name ?? "").trim() || (node.value ?? "").trim();
}

function countNodes(nodes: SnapshotNode[]): number {
  let total = 0;
  for (const node of nodes) {
    if (!META_ROLES.has(node.role)) total += 1;
    total += countNodes(node.children);
  }
  return total;
}

function normalizeNodes(nodes: SnapshotNode[]): SnapshotNode[] {
  return nodes
    .map((node) => normalizeNode(node))
    .filter((node): node is SnapshotNode => node != null);
}

function normalizeNode(node: SnapshotNode): SnapshotNode | null {
  const children = normalizeNodes(node.children);
  const role = node.role.toLowerCase();
  const label = getLabel(node);

  if ((role === "generic" || role === "paragraph") && !label && !node.value && children.length === 1) {
    return children[0];
  }

  if (TEXT_ROLES.has(role) && !label && !node.value && children.length === 0) {
    return null;
  }

  return { ...node, children };
}

function dedupeChildren(children: SnapshotNode[], parentLabel: string): SnapshotNode[] {
  const out: SnapshotNode[] = [];
  let prevTextSig = "";

  for (const child of children) {
    const role = child.role.toLowerCase();
    const label = getLabel(child);
    const textLike = TEXT_ROLES.has(role);

    if (textLike && !label && !child.value && child.children.length === 0) {
      continue;
    }
    if (textLike && parentLabel && label === parentLabel) {
      continue;
    }

    const sig = `${role}|${label}|${child.value ?? ""}|${child.attrs.ref ?? ""}`;
    if (textLike && sig === prevTextSig) {
      continue;
    }

    prevTextSig = textLike ? sig : "";
    out.push(child);
  }

  return out;
}

function parseSnapshotTree(snapshot: string): SnapshotNode[] {
  const roots: SnapshotNode[] = [];
  const stack: Array<{ depth: number; node: SnapshotNode }> = [];

  for (const line of snapshot.split("\n")) {
    const indentMatch = line.match(/^ */);
    const indent = indentMatch ? indentMatch[0].length : 0;
    const trimmed = line.trim();
    if (!trimmed.startsWith("- ")) continue;

    const parsed = parseLine(trimmed.slice(2));
    if (!parsed) continue;
    const depth = Math.floor(indent / 2);

    while (stack.length > depth) stack.pop();
    if (stack.length > 0) {
      stack[stack.length - 1].node.children.push(parsed);
    } else {
      roots.push(parsed);
    }
    stack.push({ depth, node: parsed });
  }

  return roots;
}

function parseLine(content: string): SnapshotNode | null {
  const firstSpace = content.indexOf(" ");
  const role = (firstSpace === -1 ? content : content.slice(0, firstSpace)).trim();
  if (!role) return null;

  let rest = firstSpace === -1 ? "" : content.slice(firstSpace + 1).trim();
  let name: string | undefined;
  let value: string | undefined;
  let kind: string | undefined;
  let hints: string[] = [];
  const attrs: Record<string, string> = {};

  const quoted = readQuoted(rest);
  if (quoted) {
    name = quoted.value;
    rest = quoted.rest;
  }

  if (rest.startsWith("[")) {
    const attrChunk = readBracket(rest);
    if (attrChunk) {
      Object.assign(attrs, parseAttrs(attrChunk.value));
      rest = attrChunk.rest;
    }
  }

  const kindMatch = rest.match(/^([a-zA-Z][a-zA-Z_-]*)\s+\[([^\]]+)\](.*)$/);
  if (kindMatch) {
    kind = kindMatch[1];
    hints = kindMatch[2]
      .split(",")
      .map((hint) => hint.trim())
      .filter(Boolean);
    rest = kindMatch[3].trim();
  }

  if (rest.startsWith(":")) {
    value = rest.slice(1).trim();
  } else {
    const valueIdx = rest.indexOf(": ");
    if (valueIdx >= 0) {
      value = rest.slice(valueIdx + 2).trim();
    }
  }

  return { role, name, value, kind, hints, attrs, children: [] };
}

function readQuoted(input: string): { value: string; rest: string } | null {
  if (!input.startsWith("\"")) return null;
  let escaped = false;
  for (let i = 1; i < input.length; i += 1) {
    const ch = input[i];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (ch === "\\") {
      escaped = true;
      continue;
    }
    if (ch === "\"") {
      const raw = input.slice(0, i + 1);
      const rest = input.slice(i + 1).trim();
      try {
        return { value: JSON.parse(raw) as string, rest };
      } catch {
        return { value: raw.slice(1, -1), rest };
      }
    }
  }
  return null;
}

function readBracket(input: string): { value: string; rest: string } | null {
  if (!input.startsWith("[")) return null;
  const end = input.indexOf("]");
  if (end === -1) return null;
  return {
    value: input.slice(1, end).trim(),
    rest: input.slice(end + 1).trim(),
  };
}

function parseAttrs(raw: string): Record<string, string> {
  const attrs: Record<string, string> = {};
  for (const part of raw.split(",")) {
    const token = part.trim();
    if (!token) continue;
    const eq = token.indexOf("=");
    if (eq === -1) {
      attrs[token] = "true";
      continue;
    }
    attrs[token.slice(0, eq).trim()] = token.slice(eq + 1).trim();
  }
  return attrs;
}
