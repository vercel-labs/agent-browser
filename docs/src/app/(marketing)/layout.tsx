import type { ReactNode } from 'react';
import { Footer } from '@vercel/geistdocs/footer';
import { GeistdocsThemeScript } from '@vercel/geistdocs/layout';
import { Navbar } from '@vercel/geistdocs/navbar';
import { GeistSans } from 'geist/font/sans';
import { GeistMono } from 'geist/font/mono';
import { GeistPixelSquare } from 'geist/font/pixel';
import { SpeedInsights } from '@vercel/speed-insights/next';
import { Analytics } from '@vercel/analytics/next';
import { DocsProvider } from '@/components/geistdocs-provider';
import { config } from '@/lib/geistdocs/config';
import { ProductProvider, ProductWordmark } from '@/components/landing/product-context';
import page from '../../../content/landing/home';
import '@/components/landing/landing.css';
import '@/components/navbar-labs-wordmark.css';

export { metadata, viewport } from '@/lib/site-metadata';

/** Marketing pages have their own design foundation, without the docs chat shell. */
export default function MarketingLayout({ children }: { children: ReactNode }) {
  return (
    <html
      lang="en"
      suppressHydrationWarning
      className={`${GeistSans.variable} ${GeistMono.variable} ${GeistPixelSquare.variable} antialiased`}
    >
      <head>
        <GeistdocsThemeScript />
      </head>
      <body>
        <DocsProvider>
          <ProductProvider
            browser={page.hero.browser}
            desktop={page.hero.desktop}
            defaultProduct={page.hero.defaultProduct}
          >
            <Navbar config={{ ...config, logo: <ProductWordmark /> }} />
            {children}
            <Footer />
          </ProductProvider>
        </DocsProvider>
        <SpeedInsights />
        <Analytics />
      </body>
    </html>
  );
}
