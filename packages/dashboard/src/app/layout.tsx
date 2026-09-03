import type { Metadata } from "next";
import "./globals.css";
import { Geist } from "next/font/google";
import { cn } from "@/lib/utils";
import { TooltipProvider } from "@/components/ui/tooltip";
import { JotaiProvider } from "@/store/provider";
import { ThemeProvider } from "@/components/theme-provider";

const geist = Geist({ subsets: ["latin"], variable: "--font-sans" });

export const metadata: Metadata = {
  title: "agent-browser",
  description: "Observability dashboard for agent-browser",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" className={cn("font-sans antialiased", geist.variable)} suppressHydrationWarning>
      <body>
        <ThemeProvider attribute="class" defaultTheme="dark" enableSystem disableTransitionOnChange>
          <JotaiProvider>
            <TooltipProvider>{children}</TooltipProvider>
          </JotaiProvider>
        </ThemeProvider>
      </body>
    </html>
  );
}
