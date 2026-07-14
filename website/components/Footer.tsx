import { GITHUB_URL, LICENSE_URL, RELEASES_URL } from "../lib/config";
import { Waveform } from "./Waveform";

const LINKS = [
  { label: "GitHub", href: GITHUB_URL },
  { label: "Releases", href: RELEASES_URL },
  { label: "License", href: LICENSE_URL },
  { label: "Beta waitlist", href: "#updates" },
];

export function Footer() {
  return (
    <footer className="border-t border-border">
      <div className="mx-auto flex w-full max-w-5xl flex-col gap-6 px-6 py-10 sm:flex-row sm:items-center sm:justify-between">
        <span className="flex items-center gap-2 text-sm text-muted">
          <Waveform bars={4} height={14} color="var(--color-muted)" />
          <span className="text-text">wavis</span>
          <span aria-hidden="true">·</span>
          native voice for small groups
        </span>
        <nav className="flex flex-wrap gap-x-6 gap-y-2 text-sm">
          {LINKS.map((l) => (
            <a
              key={l.label}
              href={l.href}
              target={l.href.startsWith("https://github.com/") ? "_blank" : undefined}
              rel={l.href.startsWith("https://github.com/") ? "noreferrer" : undefined}
              className="text-muted underline-offset-4 hover:text-text hover:underline"
            >
              {l.label}
            </a>
          ))}
        </nav>
      </div>
    </footer>
  );
}
