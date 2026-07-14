"use client";

import { createContext, useContext, useEffect, useState } from "react";
import { LATEST_RELEASE_URL } from "../lib/config";
import { detectPlatform, type Platform } from "../lib/os";
import { fetchLatestRelease } from "../lib/releases";

interface ReleaseState {
  version: string | null;
  urls: Record<Platform, string>;
  platform: Platform | null;
}

const FALLBACK: ReleaseState = {
  version: null,
  urls: {
    mac: LATEST_RELEASE_URL,
    windows: LATEST_RELEASE_URL,
    linux: LATEST_RELEASE_URL,
  },
  platform: null,
};

const ReleaseContext = createContext<ReleaseState>(FALLBACK);

export function useRelease() {
  return useContext(ReleaseContext);
}

// Fetches the latest release once and detects the visitor's OS, sharing both
// with the hero CTA and the download section. Renders nothing until mounted,
// so server output stays identical (links fall back to the releases page).
export function ReleaseProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<ReleaseState>(FALLBACK);

  useEffect(() => {
    const controller = new AbortController();
    setState((s) => ({ ...s, platform: detectPlatform() }));
    fetchLatestRelease(controller.signal).then((release) => {
      setState({
        version: release.version,
        urls: release.urls,
        platform: detectPlatform(),
      });
    });
    return () => controller.abort();
  }, []);

  return (
    <ReleaseContext.Provider value={state}>{children}</ReleaseContext.Provider>
  );
}
