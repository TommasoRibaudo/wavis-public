export function chatLinkTarget(href: string): '_blank' | undefined {
  try {
    const { hostname } = new URL(href);
    const normalizedHost = hostname.toLowerCase();
    return normalizedHost === 'github.com' || normalizedHost.endsWith('.github.com')
      ? '_blank'
      : undefined;
  } catch {
    return undefined;
  }
}
