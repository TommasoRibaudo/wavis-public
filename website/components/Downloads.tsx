"use client";

import { RELEASES_URL } from "../lib/config";
import type { Platform } from "../lib/os";
import { useRelease } from "./ReleaseProvider";

interface PlatformMeta {
  id: Platform;
  label: string;
  note: string;
  command: string;
}

const PLATFORMS: PlatformMeta[] = [
  { id: "mac", label: "macOS", note: "Apple Silicon · .dmg", command: "/download macos" },
  { id: "windows", label: "Windows", note: "64-bit · installer", command: "/download windows" },
  { id: "linux", label: "Linux", note: "x86-64 · AppImage", command: "/download linux" },
];

const LABELS: Record<Platform, string> = {
  mac: "macOS",
  windows: "Windows",
  linux: "Linux",
};

// Hero primary CTA. Targets the visitor's OS once detected; before that (and
// with JS off) it points at the download section so it is never a dead end.
export function PrimaryDownloadButton() {
  const { urls, platform, version } = useRelease();
  const href = platform ? urls[platform] : "#download";
  const label = platform ? `Download for ${LABELS[platform]}` : "Download Wavis";

  return (
    <div className="flex flex-col items-start gap-1.5">
      <a
        href={href}
        className="inline-flex items-center gap-2 rounded-md border border-accent bg-accent px-5 py-2.5 font-bold text-bg transition-colors hover:bg-transparent hover:text-accent"
        style={{ touchAction: "manipulation" }}
      >
        <span aria-hidden="true">▸</span>
        {label}
      </a>
      <span className="text-xs text-muted" aria-live="polite">
        {version ? `${version} · ` : ""}macOS · Windows · Linux
      </span>
    </div>
  );
}

// Download section: one always-clickable link per platform, with the visitor's
// OS highlighted. Links resolve to the real latest-release asset, falling back
// to the releases page.
export function Downloads() {
  const { urls, platform, version } = useRelease();

  return (
    <section
      id="download"
      className="mx-auto w-full max-w-5xl scroll-mt-8 px-6 py-20 sm:py-24"
    >
      <div className="mb-10">
        <p className="mb-2 text-xs uppercase tracking-widest text-muted">
          downloads
        </p>
        <h2 className="text-2xl font-bold sm:text-3xl">Get Wavis</h2>
        <p className="mt-2 max-w-xl text-muted">
          Built and signed for each platform, published to GitHub Releases.
          {version ? ` Latest build ${version}.` : ""}
        </p>
      </div>

      <ul className="grid gap-3 sm:grid-cols-3">
        {PLATFORMS.map((p) => {
          const detected = platform === p.id;
          return (
            <li key={p.id}>
              <a
                href={urls[p.id]}
                className={
                  "group flex h-full flex-col gap-3 rounded-md border p-5 transition-colors " +
                  (detected
                    ? "border-accent bg-accent/5"
                    : "border-border hover:border-accent")
                }
                style={{ touchAction: "manipulation" }}
              >
                <span className="flex items-center justify-between">
                  <span className="text-base font-bold">{p.label}</span>
                  {detected && (
                    <span className="rounded-sm border border-accent px-1.5 py-0.5 text-[10px] uppercase tracking-widest text-accent">
                      your os
                    </span>
                  )}
                </span>
                <span className="text-xs text-muted">{p.note}</span>
                <span className="mt-auto text-sm text-accent">{p.command}</span>
              </a>
            </li>
          );
        })}
      </ul>

      <p className="mt-6 text-xs text-muted">
        Looking for a specific build or older version? Browse{" "}
        <a
          href={RELEASES_URL}
          className="text-blue underline underline-offset-4 hover:text-accent"
        >
          all releases on GitHub
        </a>
        .
      </p>
    </section>
  );
}
