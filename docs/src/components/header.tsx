"use client";

import Link from "next/link";
import { useMobileNav } from "./mobile-nav-context";
import { ThemeToggle } from "./theme-toggle";

export function Header() {
  const { isOpen, toggle } = useMobileNav();

  return (
    <header className="sticky top-0 z-50 bg-background/90 backdrop-blur-sm">
      <div className="flex h-14 items-center justify-between px-4 gap-6">
        <div className="flex items-center gap-2">
          <Link href="https://vercel.com" title="Made with love by Vercel">
            <svg
              data-testid="geist-icon"
              height="18"
              strokeLinejoin="round"
              viewBox="0 0 16 16"
              width="18"
              style={{ color: "currentcolor" }}
            >
              <path
                fillRule="evenodd"
                clipRule="evenodd"
                d="M8 1L16 15H0L8 1Z"
                fill="currentColor"
              ></path>
            </svg>
          </Link>
          <span className="text-border">
            <svg
              data-testid="geist-icon"
              height="16"
              strokeLinejoin="round"
              viewBox="0 0 16 16"
              width="16"
              style={{ color: "currentcolor" }}
            >
              <path
                fillRule="evenodd"
                clipRule="evenodd"
                d="M4.01526 15.3939L4.3107 14.7046L10.3107 0.704556L10.6061 0.0151978L11.9849 0.606077L11.6894 1.29544L5.68942 15.2954L5.39398 15.9848L4.01526 15.3939Z"
                fill="currentColor"
              ></path>
            </svg>
          </span>
          <Link href="/">
            <span className="font-medium tracking-tight text-lg">
              agent-browser
            </span>
          </Link>
        </div>
        <nav className="flex items-center gap-4">
          <a
            href="https://github.com/vercel-labs/agent-browser"
            target="_blank"
            rel="noopener noreferrer"
            className="hidden sm:flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground transition-colors"
          >
            <svg
              viewBox="0 0 16 16"
              className="h-4 w-4"
              fill="currentColor"
              aria-hidden="true"
            >
              <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z" />
            </svg>
            <span>14k</span>
          </a>
          <a
            href="https://www.npmjs.com/package/agent-browser"
            target="_blank"
            rel="noopener noreferrer"
            className="hidden sm:block text-sm text-muted-foreground hover:text-foreground transition-colors"
          >
            npm
          </a>
          <ThemeToggle />
          <button
            onClick={toggle}
            className="lg:hidden p-2 -mr-2 text-muted-foreground hover:text-foreground transition-colors"
            aria-label="Toggle menu"
          >
            {isOpen ? (
              <svg className="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M6 18L18 6M6 6l12 12" />
              </svg>
            ) : (
              <svg className="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M4 6h16M4 12h16M4 18h16" />
              </svg>
            )}
          </button>
        </nav>
      </div>
    </header>
  );
}
