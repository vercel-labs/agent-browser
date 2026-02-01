"use client";

import Link from "next/link";
import { useMobileNav } from "./mobile-nav-context";

export function Header() {
  const { isOpen, toggle } = useMobileNav();

  return (
    <header className="sticky top-0 z-50 bg-black/90 backdrop-blur-sm">
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
          <span className="text-[#333]">
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
            className="hidden sm:block text-sm text-[#666] hover:text-[#999] transition-colors"
          >
            GitHub
          </a>
          <a
            href="https://www.npmjs.com/package/agent-browser"
            target="_blank"
            rel="noopener noreferrer"
            className="hidden sm:block text-sm text-[#666] hover:text-[#999] transition-colors"
          >
            npm
          </a>
          <button
            onClick={toggle}
            className="lg:hidden p-2 -mr-2 text-[#888] hover:text-white transition-colors"
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
