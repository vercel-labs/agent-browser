import { pageMetadata } from "@/lib/page-metadata";

export const metadata = pageMetadata("providers/chrome-extension");

export default function Layout({ children }: { children: React.ReactNode }) {
  return children;
}
