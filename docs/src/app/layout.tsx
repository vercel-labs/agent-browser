import type { Metadata, Viewport } from "next";
import { Footer } from "@vercel/geistdocs/footer";
import { GeistdocsThemeScript } from "@vercel/geistdocs/layout";
import { Navbar } from "@vercel/geistdocs/navbar";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";
import { GeistPixelSquare } from "geist/font/pixel";
import { cookies } from "next/headers";
import { SpeedInsights } from "@vercel/speed-insights/next";
import { Analytics } from "@vercel/analytics/next";
import { DocsProvider } from "@/components/geistdocs-provider";
import { DocsChat } from "@/components/docs-chat";
import { config } from "@/lib/geistdocs/config";
import "./globals.css";

export const viewport: Viewport = { viewportFit: "cover" };

export const metadata: Metadata = {
  metadataBase: new URL("https://agent-browser.dev"),
  title: {
    default: "agent-browser | Browser Automation for AI",
    template: "%s | agent-browser",
  },
  description: "Browser automation CLI for AI agents",
  openGraph: {
    type: "website",
    locale: "en_US",
    url: "https://agent-browser.dev",
    siteName: "agent-browser",
    title: "agent-browser | Browser Automation for AI",
    description: "Browser automation CLI for AI agents",
    images: [{ url: "/og", width: 1200, height: 630, alt: "agent-browser" }],
  },
  twitter: {
    card: "summary_large_image",
    title: "agent-browser | Browser Automation for AI",
    description: "Browser automation CLI for AI agents",
    images: ["/og"],
  },
};

export default async function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  const cookieStore = await cookies();
  const chatOpen = cookieStore.get("docs-chat-open")?.value === "true";
  const storedWidth = Number(cookieStore.get("docs-chat-width")?.value);
  const chatWidth =
    Number.isFinite(storedWidth) && storedWidth > 0
      ? Math.min(700, Math.max(300, storedWidth))
      : 400;

  return (
    <html
      lang="en"
      suppressHydrationWarning
      className={`${GeistSans.variable} ${GeistMono.variable} ${GeistPixelSquare.variable} antialiased`}
    >
      <head>
        <GeistdocsThemeScript />
        {chatOpen && (
          <style
            dangerouslySetInnerHTML={{
              __html: `@media(min-width:1280px){body{padding-right:${chatWidth}px}}`,
            }}
          />
        )}
      </head>
      <body>
        <DocsProvider>
          <Navbar config={config} />
          {children}
          <Footer />
          <DocsChat defaultOpen={chatOpen} defaultWidth={chatWidth} />
        </DocsProvider>
        <SpeedInsights />
        <Analytics />
      </body>
    </html>
  );
}
