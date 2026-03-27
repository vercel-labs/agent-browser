"use client";

import type { ReactNode } from "react";

type JsonValue = string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };

function renderValue(value: JsonValue, indent: number): ReactNode {
  if (value === null) {
    return <span className="json-null">null</span>;
  }
  if (typeof value === "boolean") {
    return <span className="json-bool">{String(value)}</span>;
  }
  if (typeof value === "number") {
    return <span className="json-number">{String(value)}</span>;
  }
  if (typeof value === "string") {
    return <span className="json-string">&quot;{value}&quot;</span>;
  }
  if (Array.isArray(value)) {
    if (value.length === 0) return <span className="json-punct">[]</span>;
    const pad = "  ".repeat(indent + 1);
    const closePad = "  ".repeat(indent);
    return (
      <>
        <span className="json-punct">[</span>
        {"\n"}
        {value.map((item, i) => (
          <span key={i}>
            {pad}
            {renderValue(item, indent + 1)}
            {i < value.length - 1 && <span className="json-punct">,</span>}
            {"\n"}
          </span>
        ))}
        {closePad}
        <span className="json-punct">]</span>
      </>
    );
  }
  if (typeof value === "object") {
    const entries = Object.entries(value);
    if (entries.length === 0) return <span className="json-punct">{"{}"}</span>;
    const pad = "  ".repeat(indent + 1);
    const closePad = "  ".repeat(indent);
    return (
      <>
        <span className="json-punct">{"{"}</span>
        {"\n"}
        {entries.map(([k, v], i) => (
          <span key={k}>
            {pad}
            <span className="json-key">&quot;{k}&quot;</span>
            <span className="json-punct">: </span>
            {renderValue(v, indent + 1)}
            {i < entries.length - 1 && <span className="json-punct">,</span>}
            {"\n"}
          </span>
        ))}
        {closePad}
        <span className="json-punct">{"}"}</span>
      </>
    );
  }
  return String(value);
}

export function JsonSyntax({ value }: { value: unknown }) {
  return <code>{renderValue(value as JsonValue, 0)}</code>;
}
