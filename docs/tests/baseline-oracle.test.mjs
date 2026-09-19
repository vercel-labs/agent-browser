import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";
import { runInNewContext } from "node:vm";
import { evaluate } from "@mdx-js/mdx";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import * as runtime from "react/jsx-runtime";
import ts from "typescript";
import {
  baseline,
  pages,
  normalize,
  text,
  renderedHeadings,
} from "./helpers.mjs";

const require = createRequire(import.meta.url);
const evaluated = { exports: {} };
runInNewContext(
  ts.transpileModule(baseline.sources["docs/mdx-components.tsx"].raw, {
    fileName: "original-mdx-components.tsx",
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText,
  {
    module: evaluated,
    exports: evaluated.exports,
    require(specifier) {
      if (specifier === "react/jsx-runtime") return require(specifier);
      if (specifier === "next/link")
        return { default: (props) => React.createElement("a", props) };
      if (specifier === "@/components/code-block")
        return {
          CodeBlock: ({ code }) => React.createElement("pre", null, code),
        };
      throw new Error(`Unexpected original component import ${specifier}`);
    },
  },
);
const components = {
  ...evaluated.exports.useMDXComponents({}),
  pre: (props) => React.createElement("pre", props),
  DiffDemo: () => null,
};

test("actual original React heading components recurse through nested inline children without deduplicating", () => {
  const children = [
    "Nested ",
    React.createElement("strong", { key: "bold" }, [
      "bold ",
      React.createElement("code", { key: "code" }, "--Flag"),
    ]),
    " & ",
    React.createElement("a", { key: "link", href: "/" }, "Link"),
    " ",
    42,
  ];
  const html = renderToStaticMarkup(
    React.createElement(
      React.Fragment,
      null,
      React.createElement(components.h2, null, children),
      React.createElement(components.h2, null, children),
    ),
  );
  assert.deepEqual(renderedHeadings(html), [
    {
      level: 2,
      text: "Nested bold --Flag & Link 42",
      id: "nested-bold---flag-link-42",
    },
    {
      level: 2,
      text: "Nested bold --Flag & Link 42",
      id: "nested-bold---flag-link-42",
    },
  ]);
});

for (const page of pages) {
  test(`${page.path}: captured content and anchors agree with independently rendered original MDX`, async () => {
    const raw = baseline.sources[page.source].raw.replace(
      /^import \{ DiffDemo \} from "@\/components\/diff-demo"\r?\n/,
      "",
    );
    const { default: Content } = await evaluate(raw, { ...runtime });
    const html = renderToStaticMarkup(
      React.createElement(Content, { components }),
    );
    assert.deepEqual(renderedHeadings(html), page.headings);
    const visible = text(html);
    for (const sample of page.content)
      assert.ok(
        visible.includes(normalize(sample)),
        `${page.path}: invalid SSR oracle sample ${JSON.stringify(sample)}`,
      );
  });
}
