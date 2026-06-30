import type { Metadata, Viewport } from "next";
import { JetBrains_Mono } from "next/font/google";
import "./globals.css";

// Self-hosted, preloaded, swap — no layout shift, no runtime font request.
const jetbrainsMono = JetBrains_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "700"],
  display: "swap",
  variable: "--font-jetbrains-mono",
});

const SITE_URL = process.env.NEXT_PUBLIC_SITE_URL ?? "https://wavis.app";
const DESCRIPTION =
  "Wavis is a native desktop app for invite-only voice rooms with low-latency audio, screen sharing, and few, obvious controls. No browser required.";

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  title: "Wavis: native voice rooms for small private groups",
  description: DESCRIPTION,
  applicationName: "Wavis",
  keywords: [
    "Wavis",
    "voice chat",
    "voice rooms",
    "screen sharing",
    "native desktop app",
    "private groups",
    "low latency audio",
    "open source",
  ],
  alternates: { canonical: "/" },
  icons: {
    icon: "/favicon.png",
    apple: "/favicon.png",
  },
  openGraph: {
    type: "website",
    url: SITE_URL,
    siteName: "Wavis",
    title: "Wavis: native voice rooms for small private groups",
    description: DESCRIPTION,
    images: [
      {
        url: "/og.png",
        width: 512,
        height: 512,
        alt: "Wavis app icon",
      },
    ],
  },
  twitter: {
    card: "summary",
    title: "Wavis: native voice rooms for small private groups",
    description: DESCRIPTION,
    images: ["/og.png"],
  },
};

export const viewport: Viewport = {
  themeColor: "#1e1e2e",
  colorScheme: "dark",
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" className={jetbrainsMono.variable}>
      <head>
        {/* The download buttons resolve real asset URLs from the GitHub API. */}
        <link rel="preconnect" href="https://api.github.com" />
      </head>
      <body>{children}</body>
    </html>
  );
}
