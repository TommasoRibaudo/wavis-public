import { GITHUB_URL } from "../lib/config";
import { AppMock } from "./AppMock";
import { PrimaryDownloadButton } from "./Downloads";
import { Waveform } from "./Waveform";

export function Hero() {
  return (
    <header className="mx-auto w-full max-w-6xl px-6 pb-16 pt-12 sm:pt-16">
      <nav className="mb-14 flex items-center justify-between">
        <span className="flex items-center gap-2 text-lg font-bold">
          <Waveform bars={4} height={16} />
          wavis
        </span>
        <a
          href={GITHUB_URL}
          className="text-sm text-muted underline-offset-4 hover:text-text hover:underline"
        >
          github
        </a>
      </nav>

      <div className="grid items-center gap-12 lg:grid-cols-[1.05fr_1fr]">
        <div>
          <p className="mb-4 text-xs uppercase tracking-[0.2em] text-muted">
            native voice · invite-only · open source
          </p>
          <h1 className="text-4xl font-bold leading-[1.1] sm:text-5xl">
            Native voice rooms for small private groups.
          </h1>
          <p className="mt-5 max-w-xl text-lg text-muted">
            Invite-only rooms, low-latency audio, screen sharing, and few,
            obvious controls. No browser required.
          </p>

          <div className="mt-8 flex flex-wrap items-center gap-3">
            <PrimaryDownloadButton />
            <a
              href={GITHUB_URL}
              className="inline-flex items-center gap-2 rounded-md border border-border px-5 py-2.5 transition-colors hover:border-blue hover:text-blue"
              style={{ touchAction: "manipulation" }}
            >
              View on GitHub
            </a>
            <a
              href="#updates"
              className="inline-flex items-center gap-2 px-2 py-2.5 text-muted underline-offset-4 hover:text-text hover:underline"
            >
              Get release updates
            </a>
          </div>
        </div>

        <div className="lg:pl-4">
          <AppMock />
        </div>
      </div>
    </header>
  );
}
