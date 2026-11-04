import { GeistdocsDocsLayout } from '@vercel/geistdocs/layout';
import { notFound } from 'next/navigation';
import type { ReactNode } from 'react';
import type { Root, Node } from 'fumadocs-core/page-tree';
import { config } from '@/lib/geistdocs/config';
import { geistdocsSource } from '@/lib/geistdocs/source';

function docsTree(tree: Root): Root {
  function remap(node: Node): Node {
    if (node.type === 'page') return node.url === '/' ? { ...node, url: '/docs' } : node;
    if (node.type === 'folder')
      return {
        ...node,
        index: node.index ? (remap(node.index) as typeof node.index) : undefined,
        children: node.children.map(remap),
      };
    return node;
  }
  return { ...tree, children: tree.children.map(remap) };
}

export default async function DocsLayout({
  children,
  params,
}: {
  children: ReactNode;
  params: Promise<{ lang: string }>;
}) {
  const { lang } = await params;
  if (lang !== 'en') notFound();
  return (
    <GeistdocsDocsLayout
      config={config}
      tree={docsTree(geistdocsSource.source.getPageTree(lang))}
      containerProps={{ className: 'mx-auto max-w-[1448px]' }}
    >
      {children}
    </GeistdocsDocsLayout>
  );
}

export const generateStaticParams = () => [{ lang: 'en' }];
