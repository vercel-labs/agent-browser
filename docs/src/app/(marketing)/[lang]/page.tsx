import { notFound } from 'next/navigation';
import page from '../../../../content/landing/home';
import { AgentClosing } from '@/components/landing/agent-closing';
import { AgentHero } from '@/components/landing/agent-hero';
import { Container } from '@/components/landing/compat';
import { AgentTweetWall } from '@/components/landing/agent-tweet-wall';
import { AgentUseCases, type AgentUseCasesProps } from '@/components/landing/agent-use-cases';
import { pageMetadata } from '@/lib/page-metadata';

const baseMetadata = pageMetadata('');
export const metadata = {
  ...baseMetadata,
  title: { absolute: page.metadata.title },
  description: page.metadata.description,
  openGraph: {
    ...baseMetadata.openGraph,
    title: page.metadata.title,
    description: page.metadata.description,
  },
  twitter: {
    ...baseMetadata.twitter,
    title: page.metadata.title,
    description: page.metadata.description,
  },
};

export default async function Home({ params }: { params: Promise<{ lang: string }> }) {
  if ((await params).lang !== 'en') notFound();
  return (
    <main className="agent-landing" id="main-content">
      <Container>
        <AgentHero />
        <AgentUseCases {...(page.useCases as AgentUseCasesProps)} />
        <AgentTweetWall {...page.tweets} />
        <AgentClosing {...page.closing} />
      </Container>
    </main>
  );
}

export const generateStaticParams = () => [{ lang: 'en' }];
