"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useMobileNav } from "./mobile-nav-context";

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
  const { isOpen, setIsOpen } = useMobileNav();

  return (
    <>
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
          fixed lg:sticky top-14 left-0 z-50 lg:z-auto
          w-56 lg:w-48 h-[calc(100vh-3.5rem)]
          bg-black
          transform transition-transform duration-150 ease-out
          ${isOpen ? "translate-x-0" : "-translate-x-full lg:translate-x-0"}
        `}
      >
        <div className="h-full overflow-y-auto py-5 pl-3 pr-5">
          <nav className="space-y-0.5">
            {navigation.map((item) => {
              const isActive = pathname === item.href;

              return (
                <Link
                  key={item.name}
                  href={item.href}
                  className={`block px-2 py-1.5 text-sm transition-colors ${
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
        </div>
      </aside>
    </>
  );
}
