"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { cn } from "@/lib/utils";
import { navigation } from "@/lib/docs-navigation";

export function DocsSidebar() {
  const pathname = usePathname();

  return (
    <nav className="space-y-6 pb-8">
      {navigation.map((section, sectionIndex) => (
        <div key={section.title ?? sectionIndex}>
          {section.title && (
            <h4 className="text-xs font-normal text-muted-foreground/50 uppercase tracking-wider mb-2">
              {section.title}
            </h4>
          )}
          <ul className="space-y-1">
            {section.items.map((item) => {
              const isActive = pathname === item.href;
              return (
                <li key={item.href}>
                  <Link
                    href={item.href}
                    className={cn(
                      "text-sm transition-colors block py-1",
                      isActive
                        ? "text-primary font-medium"
                        : "text-muted-foreground hover:text-foreground",
                    )}
                  >
                    {item.name}
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}
