import type { Metadata, Viewport } from 'next';

export const viewport: Viewport = { viewportFit: 'cover' };

export const metadata: Metadata = {
  metadataBase: new URL('https://agent-browser.dev'),
  title: {
    default: 'agent-browser | Browser Automation for AI',
    template: '%s | agent-browser',
  },
  description: 'Browser automation CLI for AI agents',
  openGraph: {
    type: 'website',
    locale: 'en_US',
    url: 'https://agent-browser.dev',
    siteName: 'agent-browser',
    title: 'agent-browser | Browser Automation for AI',
    description: 'Browser automation CLI for AI agents',
    images: [{ url: '/og', width: 1200, height: 630, alt: 'agent-browser' }],
  },
  twitter: {
    card: 'summary_large_image',
    title: 'agent-browser | Browser Automation for AI',
    description: 'Browser automation CLI for AI agents',
    images: ['/og'],
  },
};
