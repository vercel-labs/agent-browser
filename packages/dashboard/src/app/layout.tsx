import type { Metadata } from "next";
import "./globals.css";
import { Geist } from "next/font/google";
import { cn } from "@/lib/utils";
import { TooltipProvider } from "@/components/ui/tooltip";
import { JotaiProvider } from "@/store/provider";

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
    <html lang="en" className={cn("dark font-sans antialiased", geist.variable)}>
      <body>
        <JotaiProvider>
          <TooltipProvider>{children}</TooltipProvider>
        </JotaiProvider>
      </body>
    </html>
  );
}
