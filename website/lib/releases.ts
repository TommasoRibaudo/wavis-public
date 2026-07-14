import { LATEST_RELEASE_API, LATEST_RELEASE_URL } from "./config";
import type { Platform } from "./os";

export interface ReleaseAssets {
  /** Release tag, e.g. "desktop-v0.1.8". Null when the API is unavailable. */
  version: string | null;
  /** Per-platform direct download URLs, falling back to the releases page. */
  urls: Record<Platform, string>;
}

interface GitHubAsset {
  name: string;
  browser_download_url: string;
}

interface GitHubRelease {
  tag_name: string;
  assets: GitHubAsset[];
}

// Release filenames embed the version (e.g. Wavis_0.1.8_aarch64.dmg), so static
// URLs would break each release — match the current asset names by suffix.
const MATCHERS: Record<Platform, RegExp> = {
  mac: /\.dmg$/i,
  windows: /-setup\.exe$/i,
  linux: /\.AppImage$/i,
};

function fallbackUrls(): Record<Platform, string> {
  return {
    mac: LATEST_RELEASE_URL,
    windows: LATEST_RELEASE_URL,
    linux: LATEST_RELEASE_URL,
  };
}

/**
 * Resolve direct download URLs for the latest release. On any failure (network,
 * rate limit, missing asset) the platform falls back to the releases page, so a
 * download is always reachable.
 */
export async function fetchLatestRelease(
  signal?: AbortSignal,
): Promise<ReleaseAssets> {
  const urls = fallbackUrls();
  try {
    const res = await fetch(LATEST_RELEASE_API, {
      signal,
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!res.ok) return { version: null, urls };

    const release = (await res.json()) as GitHubRelease;
    for (const platform of Object.keys(MATCHERS) as Platform[]) {
      const asset = release.assets.find((a) => MATCHERS[platform].test(a.name));
      if (asset) urls[platform] = asset.browser_download_url;
    }
    return { version: release.tag_name ?? null, urls };
  } catch {
    return { version: null, urls };
  }
}
