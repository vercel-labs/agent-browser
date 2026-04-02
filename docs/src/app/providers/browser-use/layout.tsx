import { pageMetadata } from "@/lib/page-metadata";

export const metadata = pageMetadata("providers/browser-use");

export default function Layout({ children }: { children: React.ReactNode }) {
  return children;
}
