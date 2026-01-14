import { codeToHtml } from "shiki";
import { CopyButton } from "./copy-button";

interface CodeBlockProps {
  code: string;
  lang?: string;
}

export async function CodeBlock({ code, lang = "bash" }: CodeBlockProps) {
  const trimmedCode = code.trim();
  const html = await codeToHtml(trimmedCode, {
    lang,
    theme: "github-dark-default",
  });

  return (
    <div className="code-block relative group">
      <CopyButton code={trimmedCode} />
      <div dangerouslySetInnerHTML={{ __html: html }} />
    </div>
  );
}
