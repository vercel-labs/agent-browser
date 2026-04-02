import { pageMetadata } from "@/lib/page-metadata";

export const metadata = pageMetadata("quick-start");

export default function Layout({ children }: { children: React.ReactNode }) {
  return children;
}
