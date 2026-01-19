"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useState, useEffect } from "react";

const navigation = [
  { name: "Introduction", href: "/" },
  { name: "Installation", href: "/installation" },
  { name: "Quick Start", href: "/quick-start" },
  { name: "Commands", href: "/commands" },
  { name: "Selectors", href: "/selectors" },
  { name: "Sessions", href: "/sessions" },
  { name: "Snapshots", href: "/snapshots" },
  { name: "Streaming", href: "/streaming" },
  { name: "Agent Mode", href: "/agent-mode" },
  { name: "CDP Mode", href: "/cdp-mode" },
  { name: "Changelog", href: "/changelog" },
];

export function Sidebar() {
  const pathname = usePathname();
  const [isOpen, setIsOpen] = useState(false);

  useEffect(() => {
    setIsOpen(false);
  }, [pathname]);

  useEffect(() => {
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("keydown", handleEscape);
    return () => document.removeEventListener("keydown", handleEscape);
  }, []);

  return (
    <>
      {/* Mobile header */}
      <header className="lg:hidden fixed top-0 left-0 right-0 z-50 bg-black/90 backdrop-blur-sm border-b border-[#222] px-4 py-3">
        <div className="flex items-center justify-between">
          <Link href="/" className="text-sm font-medium">
            agent-browser
          </Link>
          <button
            onClick={() => setIsOpen(!isOpen)}
            className="p-2 -mr-2 text-[#888] hover:text-white transition-colors"
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
        </div>
      </header>

      {/* Mobile overlay */}
      {isOpen && (
        <div
          className="lg:hidden fixed inset-0 z-40 bg-black/80"
          onClick={() => setIsOpen(false)}
        />
      )}

      {/* Sidebar */}
      <aside
        className={`
          fixed lg:sticky top-0 left-0 z-50 lg:z-auto
          w-56 lg:w-48 h-screen
          bg-black border-r border-[#222]
          transform transition-transform duration-150 ease-out
          ${isOpen ? "translate-x-0" : "-translate-x-full lg:translate-x-0"}
          pt-14 lg:pt-0
        `}
      >
        <div className="h-full overflow-y-auto p-5">
          {/* Desktop header */}
          <div className="mb-8 hidden lg:block">
            <Link href="/" className="text-sm font-medium">
              agent-browser
            </Link>
          </div>

          <nav className="space-y-0.5">
            {navigation.map((item) => {
              const isActive = pathname === item.href;

              return (
                <Link
                  key={item.name}
                  href={item.href}
                  className={`block px-2 py-1.5 text-[13px] transition-colors ${
                    isActive
                      ? "text-white"
                      : "text-[#666] hover:text-[#999]"
                  }`}
                >
                  {item.name}
                </Link>
              );
            })}
          </nav>

          <div className="mt-8 pt-4 border-t border-[#222] space-y-0.5">
            <a
              href="https://github.com/vercel-labs/agent-browser"
              target="_blank"
              rel="noopener noreferrer"
              className="block px-2 py-1.5 text-[13px] text-[#666] hover:text-[#999] transition-colors"
            >
              GitHub
            </a>
            <a
              href="https://www.npmjs.com/package/agent-browser"
              target="_blank"
              rel="noopener noreferrer"
              className="block px-2 py-1.5 text-[13px] text-[#666] hover:text-[#999] transition-colors"
            >
              npm
            </a>
          </div>
        </div>
      </aside>
    </>
  );
}
