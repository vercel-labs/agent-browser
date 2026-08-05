import { pageMetadata } from "@/lib/page-metadata";

export const metadata = pageMetadata("changelog");

export default function Layout({ children }: { children: React.ReactNode }) {
  return children;
}
