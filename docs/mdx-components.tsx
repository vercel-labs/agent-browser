import type { MDXComponents } from "mdx/types";
import Link from "next/link";
import { CodeBlock } from "@/components/code-block";

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-")
    .trim();
}

function extractText(children: React.ReactNode): string {
  if (typeof children === "string") return children;
  if (typeof children === "number") return String(children);
  if (Array.isArray(children)) return children.map(extractText).join("");
  if (children && typeof children === "object") {
    const obj = children as unknown as Record<string, unknown>;
    if ("props" in obj) {
      const props = obj.props as { children?: React.ReactNode } | undefined;
      return extractText(props?.children);
    }
  }
  return "";
}

export function useMDXComponents(components: MDXComponents): MDXComponents {
  return {
    ...components,
    h2: ({ children }: { children?: React.ReactNode }) => {
      const id = slugify(extractText(children));
      return <h2 id={id}>{children}</h2>;
    },
    h3: ({ children }: { children?: React.ReactNode }) => {
      const id = slugify(extractText(children));
      return <h3 id={id}>{children}</h3>;
    },
    a: ({
      href,
      children,
    }: {
      href?: string;
      children?: React.ReactNode;
    }) => {
      if (href?.startsWith("/")) {
        return <Link href={href}>{children}</Link>;
      }
      return (
        <a href={href} target="_blank" rel="noopener noreferrer">
          {children}
        </a>
      );
    },
    code: ({
      children,
      className,
    }: {
      children?: React.ReactNode;
      className?: string;
    }) => {
      if (className) {
        return <code className={className}>{children}</code>;
      }
      return <code>{children}</code>;
    },
    pre: async ({ children }: { children?: React.ReactNode }) => {
      const codeElement = children as React.ReactElement<{
        className?: string;
        children?: string;
      }>;
      const className = codeElement?.props?.className || "";
      const lang = className.replace("language-", "") || "bash";
      const code = codeElement?.props?.children || "";

      return (
        <CodeBlock
          code={typeof code === "string" ? code : String(code)}
          lang={lang}
        />
      );
    },
  };
}
