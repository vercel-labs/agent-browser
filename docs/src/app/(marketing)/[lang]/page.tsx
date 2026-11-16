import { notFound } from 'next/navigation';
import page from '../../../../content/landing/home';
import { AgentClosing } from '@/components/landing/agent-closing';
import { AgentHero } from '@/components/landing/agent-hero';
import { Container } from '@/components/landing/compat';
import { AgentTweetWall } from '@/components/landing/agent-tweet-wall';
import { AgentUseCaseTabs, type AgentUseCasesProps } from '@/components/landing/agent-use-case-tabs';
import { pageMetadata } from '@/lib/page-metadata';
import { siteUrl } from '@/lib/site';

const baseMetadata = pageMetadata('');
const homeOgImage = '/landing/home-og.png';
// Messages can download and autoplay a direct MP4 linked from og:video (Apple TN3156).
// The file name carries a content hash so link preview caches pick up new cuts.
// Next.js does not resolve og:video against metadataBase, so the URL is absolute.
const homeOgVideo = `${siteUrl}/landing/home-og-c3c73ab9.mp4`;
export const metadata = {
  ...baseMetadata,
  title: { absolute: page.metadata.title },
  description: page.metadata.description,
  openGraph: {
    ...baseMetadata.openGraph,
    title: page.metadata.title,
    description: page.metadata.description,
    images: [
      {
        url: homeOgImage,
        width: 2400,
        height: 1256,
        alt: 'Browser automation for any agent',
      },
    ],
    videos: [
      {
        url: homeOgVideo,
        secureUrl: homeOgVideo,
        type: 'video/mp4',
        width: 1200,
        height: 848,
      },
    ],
  },
  twitter: {
    ...baseMetadata.twitter,
    title: page.metadata.title,
    description: page.metadata.description,
    images: [homeOgImage],
  },
};

export default async function Home({ params }: { params: Promise<{ lang: string }> }) {
  if ((await params).lang !== 'en') notFound();
  return (
    <main className="agent-landing" id="main-content">
      <Container>
        <AgentHero description={page.hero.description} />
        <AgentUseCaseTabs {...(page.useCases as AgentUseCasesProps)} />
        <AgentTweetWall {...page.tweets} />
        <AgentClosing {...page.closing} />
      </Container>
    </main>
  );
}

export const generateStaticParams = () => [{ lang: 'en' }];
