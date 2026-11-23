import type { Metadata } from 'next';
import DocsPage from '../[...slug]/page';
import { pageMetadata } from '@/lib/page-metadata';

export const metadata: Metadata = pageMetadata('introduction');

export default async function Introduction({ params }: { params: Promise<{ lang: string }> }) {
  return <DocsPage params={params} />;
}
