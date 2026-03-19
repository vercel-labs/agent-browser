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
      { name: "Skills", href: "/skills" },
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
      { name: "Security", href: "/security" },
      { name: "Next.js + Vercel", href: "/next" },
      { name: "Native Mode", href: "/native-mode" },
    ],
  },
  {
    title: "Providers",
    items: [
      { name: "Browser Use", href: "/providers/browser-use" },
      { name: "Browserbase", href: "/providers/browserbase" },
      { name: "Browserless", href: "/providers/browserless" },
      { name: "Kernel", href: "/providers/kernel" },
    ],
  },
  {
    title: "Engines",
    items: [
      { name: "Chrome", href: "/engines/chrome" },
      { name: "Lightpanda", href: "/engines/lightpanda" },
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
