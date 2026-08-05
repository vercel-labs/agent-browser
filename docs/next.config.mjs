import createMDX from "@next/mdx";

/** @type {import('next').NextConfig} */
const nextConfig = {
  pageExtensions: ["js", "jsx", "ts", "tsx", "md", "mdx"],
  serverExternalPackages: ["just-bash", "bash-tool"],
};

const withMDX = createMDX({});

export default withMDX(nextConfig);
