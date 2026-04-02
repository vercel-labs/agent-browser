import { pageMetadata } from "@/lib/page-metadata";

export const metadata = pageMetadata("engines/chrome");

export default function Layout({ children }: { children: React.ReactNode }) {
  return children;
}
