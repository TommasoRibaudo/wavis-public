export type Platform = "mac" | "windows" | "linux";

interface UADataNavigator extends Navigator {
  userAgentData?: { platform?: string };
}

/**
 * Best-effort guess of the visitor's desktop OS, used only to highlight the
 * matching download. Never the sole path to a download — every platform link
 * is always present and clickable.
 */
export function detectPlatform(): Platform | null {
  if (typeof navigator === "undefined") return null;

  const nav = navigator as UADataNavigator;
  const hint = (
    nav.userAgentData?.platform ??
    nav.platform ??
    navigator.userAgent ??
    ""
  ).toLowerCase();

  if (hint.includes("mac") || hint.includes("darwin") || hint.includes("iphone") || hint.includes("ipad")) {
    return "mac";
  }
  if (hint.includes("win")) return "windows";
  if (hint.includes("linux") || hint.includes("android") || hint.includes("x11")) {
    return "linux";
  }
  return null;
}
