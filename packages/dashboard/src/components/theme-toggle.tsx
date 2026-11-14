"use client";

import { useEffect, useState } from "react";
import { useTheme } from "next-themes";
import { Monitor, Moon, Sun } from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

const THEMES = [
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
  { value: "system", label: "System", icon: Monitor },
] as const;

export function ThemeToggle() {
  const { theme, setTheme, resolvedTheme } = useTheme();
  const [mounted, setMounted] = useState(false);

  // next-themes resolves the theme on the client, so wait until mounted to
  // render the active-state icon and avoid a hydration mismatch.
  useEffect(() => setMounted(true), []);

  // Reflect the user's selection: the monitor icon when following the system
  // preference, otherwise the icon for the currently resolved theme.
  const TriggerIcon =
    !mounted || theme === "system"
      ? Monitor
      : resolvedTheme === "dark"
        ? Moon
        : Sun;

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        type="button"
        className="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none"
        title="Change theme"
      >
        <TriggerIcon className="size-3" />
        <span className="sr-only">Change theme</span>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-32">
        <DropdownMenuRadioGroup
          value={mounted ? theme : undefined}
          onValueChange={setTheme}
        >
          {THEMES.map(({ value, label, icon: Icon }) => (
            <DropdownMenuRadioItem key={value} value={value}>
              <Icon className="size-3.5" />
              {label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
