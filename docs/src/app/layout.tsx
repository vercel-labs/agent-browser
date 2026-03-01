import type { Metadata } from "next";
import { Inter, Geist_Mono } from "next/font/google";
import { GeistPixelSquare } from "geist/font/pixel";
import "./globals.css";
import { ThemeProvider } from "@/components/theme-provider";
import { Header } from "@/components/header";
import { DocsSidebar } from "@/components/docs-sidebar";
import { DocsMobileNav } from "@/components/docs-mobile-nav";
import { CopyPageButton } from "@/components/copy-page-button";
import { DocsChat } from "@/components/docs-chat";
import { cookies } from "next/headers";
import { SpeedInsights } from "@vercel/speed-insights/next";
import { Analytics } from "@vercel/analytics/next";

const inter = Inter({
  variable: "--font-inter",
  subsets: ["latin"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

export const metadata: Metadata = {
  metadataBase: new URL("https://agent-browser.dev"),
  title: {
    default: "agent-browser | Headless Browser Automation for AI",
    template: "%s | agent-browser",
  },
  description: "Headless browser automation CLI for AI agents",
  openGraph: {
    type: "website",
    locale: "en_US",
    url: "https://agent-browser.dev",
    siteName: "agent-browser",
    title: "agent-browser | Headless Browser Automation for AI",
    description: "Headless browser automation CLI for AI agents",
    images: [{ url: "/og", width: 1200, height: 630, alt: "agent-browser" }],
  },
  twitter: {
    card: "summary_large_image",
    title: "agent-browser | Headless Browser Automation for AI",
    description: "Headless browser automation CLI for AI agents",
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
  const chatWidth = Number(cookieStore.get("docs-chat-width")?.value) || 400;

  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        {chatOpen && (
          <style
            dangerouslySetInnerHTML={{
              __html: `@media(min-width:640px){body{padding-right:${chatWidth}px}}`,
            }}
          />
        )}
      </head>
      <body
        className={`${inter.variable} ${geistMono.variable} ${GeistPixelSquare.variable} bg-white text-neutral-900 antialiased dark:bg-neutral-950 dark:text-neutral-100`}
      >
        <ThemeProvider>
          <Header />
          <DocsMobileNav />
          <div className="max-w-5xl mx-auto px-6 py-8 lg:py-12 flex gap-16">
            <aside className="w-48 shrink-0 hidden lg:block sticky top-28 h-[calc(100vh-7rem)] overflow-y-auto">
              <DocsSidebar />
            </aside>
            <div className="flex-1 min-w-0 max-w-2xl pb-20">
              <div className="flex justify-end mb-4">
                <CopyPageButton />
              </div>
              <article className="prose">{children}</article>
            </div>
          </div>
          <DocsChat defaultOpen={chatOpen} defaultWidth={chatWidth} />
        </ThemeProvider>
        <SpeedInsights />
        <Analytics />
      </body>
    </html>
  );
}
