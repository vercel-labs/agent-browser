import type { Metadata } from 'next';
import DocsPage from '../[...slug]/page';
import { pageMetadata } from '@/lib/page-metadata';
import { canonicalUrlFor } from '@/lib/site';

const baseMetadata = pageMetadata('');

export const metadata: Metadata = {
  ...baseMetadata,
  title: 'Introduction',
  openGraph: {
    ...baseMetadata.openGraph,
    title: 'Introduction | agent-browser',
    url: canonicalUrlFor('/docs'),
  },
  twitter: { ...baseMetadata.twitter, title: 'Introduction | agent-browser' },
  alternates: {
    canonical: canonicalUrlFor('/docs'),
    types: { 'text/markdown': '/index.md' },
  },
};

export default async function Introduction({ params }: { params: Promise<{ lang: string }> }) {
  return <DocsPage params={params} />;
}
