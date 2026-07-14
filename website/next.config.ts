import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // Static HTML export → uploadable to S3 + CloudFront, fully pre-rendered for SEO.
  output: "export",
  // This site has its own lockfile; pin tracing here so Next doesn't pick the
  // monorepo root when it detects multiple lockfiles.
  outputFileTracingRoot: __dirname,
  // CloudFront/S3 serve directory-style URLs; trailing slash maps /path → /path/index.html.
  trailingSlash: true,
  // next/image optimization needs a server; the export is static, so serve images as-is.
  images: { unoptimized: true },
};

export default nextConfig;
