import { pageMetadata } from "@/lib/page-metadata";

export const metadata = pageMetadata("cdp-mode");

export default function Layout({ children }: { children: React.ReactNode }) {
  return children;
}
