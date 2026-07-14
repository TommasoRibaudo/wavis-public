"use client";

import { useState } from "react";
import { GITHUB_URL, RELEASES_URL } from "../lib/config";
import type { Platform } from "../lib/os";
import { useRelease } from "./ReleaseProvider";

interface PlatformMeta {
  id: Platform;
  label: string;
  note: string;
  command: string;
}

const PLATFORMS: PlatformMeta[] = [
  {
    id: "mac",
    label: "macOS",
    note: "Apple Silicon \u00b7 .dmg",
    command: "/download macos",
  },
  {
    id: "windows",
    label: "Windows",
    note: "64-bit \u00b7 installer",
    command: "/download windows",
  },
  {
    id: "linux",
    label: "Linux",
    note: "x86-64 \u00b7 AppImage",
    command: "/download linux",
  },
];

function DownloadAccessPanel({ onDismiss }: { onDismiss: () => void }) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-crust/80 px-4 py-8"
      onClick={onDismiss}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="download-access-title"
        className="w-full max-w-2xl rounded-md border border-blue bg-panel p-5 text-sm shadow-2xl"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4">
          <p id="download-access-title" className="font-bold text-blue">
            Before you connect Wavis
          </p>
          <button
            type="button"
            onClick={onDismiss}
            className="rounded-sm border border-border px-2 py-0.5 text-xs text-muted transition-colors hover:border-blue hover:text-blue"
          >
            Close
          </button>
        </div>
        <p className="mt-3 text-muted">
          The app needs a join link to connect to a room.{" "}
          <a
            href="#updates"
            className="text-blue underline underline-offset-4 hover:text-accent"
          >
            Get early access
          </a>{" "}
          to receive one, or self-host your own server — see{" "}
          <a
            href={GITHUB_URL}
            target="_blank"
            rel="noreferrer"
            className="text-blue underline underline-offset-4 hover:text-accent"
          >
            the repo
          </a>{" "}
          for setup instructions.
        </p>
      </div>
    </div>
  );
}

// Download section: one always-clickable link per platform, with the visitor's
// OS highlighted. Links resolve to the real latest-release asset, falling back
// to the releases page when JavaScript is disabled.
export function Downloads() {
  const { urls, platform, version } = useRelease();
  const [showAccessPanel, setShowAccessPanel] = useState(false);

  function handleDownloadClick() {
    setShowAccessPanel(true);
  }

  return (
    <section
      id="download"
      className="mx-auto w-full max-w-5xl scroll-mt-8 px-6 pb-20 pt-8 sm:pb-24 sm:pt-10"
    >
      <div className="mb-10">
        <p className="mb-2 text-xs uppercase tracking-widest text-muted">
          download
        </p>
        <h2 className="text-2xl font-bold sm:text-3xl">Get Wavis</h2>
        <p className="mt-2 max-w-xl text-muted">
          You&rsquo;ll need an invite link to connect.{" "}
          <a href="#updates" className="text-blue underline hover:text-accent">
            Get early access
          </a>{" "}
          below, or self-host your own server. Once you have a link, grab the
          app below.
        </p>
      </div>

      {showAccessPanel && (
        <DownloadAccessPanel
          onDismiss={() => setShowAccessPanel(false)}
        />
      )}

      <ul className="grid gap-3 sm:grid-cols-3">
        {PLATFORMS.map((p) => {
          const detected = platform === p.id;
          return (
            <li key={p.id}>
              <a
                href={urls[p.id]}
                onClick={handleDownloadClick}
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
          target="_blank"
          rel="noreferrer"
          className="text-blue underline underline-offset-4 hover:text-accent"
        >
          all releases on GitHub
        </a>
        .
      </p>
    </section>
  );
}
