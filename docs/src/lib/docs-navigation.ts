export type NavItem = {
  name: string;
  href: string;
};

export type NavSection = {
  title: string | null;
  items: NavItem[];
};

export const navigation: NavSection[] = [
  {
    title: null,
    items: [
      { name: "Introduction", href: "/" },
      { name: "Installation", href: "/installation" },
      { name: "Quick Start", href: "/quick-start" },
    ],
  },
  {
    title: "Reference",
    items: [
      { name: "Commands", href: "/commands" },
      { name: "Configuration", href: "/configuration" },
      { name: "Selectors", href: "/selectors" },
      { name: "Snapshots", href: "/snapshots" },
    ],
  },
  {
    title: "Features",
    items: [
      { name: "Sessions", href: "/sessions" },
      { name: "Diffing", href: "/diffing" },
      { name: "CDP Mode", href: "/cdp-mode" },
      { name: "Streaming", href: "/streaming" },
      { name: "Profiler", href: "/profiler" },
      { name: "iOS Simulator", href: "/ios" },
    ],
  },
  {
    title: null,
    items: [{ name: "Changelog", href: "/changelog" }],
  },
];

export const allDocsPages: NavItem[] = navigation.flatMap(
  (section) => section.items
);
