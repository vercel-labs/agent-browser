import { notFound } from 'next/navigation';
import page from '../../../../content/landing/home';
import { AgentClosing } from '@/components/landing/agent-closing';
import { AgentHero } from '@/components/landing/agent-hero';
import { Container } from '@/components/landing/compat';
import { AgentTweetWall } from '@/components/landing/agent-tweet-wall';
import { AgentUseCaseTabs, type AgentUseCasesProps } from '@/components/landing/agent-use-case-tabs';
import { pageMetadata } from '@/lib/page-metadata';

const baseMetadata = pageMetadata('');
const homeOgImage = '/landing/home-og.png';
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
