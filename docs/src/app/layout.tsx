import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import "./globals.css";
import { ThemeProvider } from "@/components/theme-provider";
import { MobileNavProvider } from "@/components/mobile-nav-context";
import { Header } from "@/components/header";
import { Sidebar } from "@/components/sidebar";
import { DocsChat } from "@/components/docs-chat";
import { cookies } from "next/headers";
import { SpeedInsights } from "@vercel/speed-insights/next";
import { Analytics } from "@vercel/analytics/next";

const geist = Geist({
  variable: "--font-geist",
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
        className={`${geist.variable} ${geistMono.variable} antialiased bg-background text-foreground`}
      >
        <ThemeProvider>
          <MobileNavProvider>
            <Header />
            <div className="flex min-h-[calc(100vh-3.5rem)]">
              <Sidebar />
              <main className="flex-1 overflow-auto">
                <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
                  <div className="prose">
                    {children}
                  </div>
                </div>
              </main>
            </div>
            <DocsChat defaultOpen={chatOpen} defaultWidth={chatWidth} />
          </MobileNavProvider>
        </ThemeProvider>
        <SpeedInsights />
        <Analytics />
      </body>
    </html>
  );
}
