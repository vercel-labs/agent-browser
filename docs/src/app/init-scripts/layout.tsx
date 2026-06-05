import { pageMetadata } from "@/lib/page-metadata";

export const metadata = pageMetadata("init-scripts");

export default function Layout({ children }: { children: React.ReactNode }) {
  return children;
}
