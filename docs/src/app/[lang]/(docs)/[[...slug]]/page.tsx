import { MobileDocsBar } from "@vercel/geistdocs/mobile-docs-bar";
import { createDocsPage } from "@vercel/geistdocs/pages/docs";
import { notFound } from "next/navigation";
import { isSafePathSegments } from "@/lib/docs-source";
import { config } from "@/lib/geistdocs/config";
import { geistdocsSource } from "@/lib/geistdocs/source";
import { pageMetadata } from "@/lib/page-metadata";
import { legacyHeadingId } from "@/lib/remark-legacy-headings";

type PageProps = {
  params: Promise<{ lang: string; slug?: string[] }>;
};

async function validate(params: PageProps["params"]) {
  const resolved = await params;
  if (resolved.lang !== "en" || !isSafePathSegments(resolved.slug ?? []))
    notFound();
  try {
    if (!geistdocsSource.source.getPage(resolved.slug, resolved.lang))
      notFound();
  } catch (error) {
    if (error instanceof URIError) notFound();
    throw error;
  }
  return resolved;
}

const docsPage = createDocsPage({
  config,
  source: geistdocsSource,
  renderTop: ({ data }) => (
    <>
      <span id={legacyHeadingId(data.title)} />
      <MobileDocsBar toc={data.toc} />
    </>
  ),
});

export default async function Page({ params }: PageProps) {
  return <docsPage.Page params={Promise.resolve(await validate(params))} />;
}

export async function generateMetadata({ params }: PageProps) {
  const { slug = [] } = await validate(params);
  return pageMetadata(slug.join("/"));
}

export const generateStaticParams = docsPage.generateStaticParams;
