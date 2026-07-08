import { GITHUB_URL } from "../lib/config";
import { AppMock } from "./AppMock";
import { Waveform } from "./Waveform";

export function Hero() {
  return (
    <header className="mx-auto w-full max-w-7xl px-6 pb-16 pt-6 sm:pt-8">
      <nav className="mb-10 flex items-center justify-between">
        <span className="flex items-center gap-2 text-lg font-bold">
          <Waveform bars={4} height={16} />
          wavis
        </span>
        <a
          href={GITHUB_URL}
          target="_blank"
          rel="noreferrer"
          className="text-sm text-muted underline-offset-4 hover:text-text hover:underline"
        >
          github
        </a>
      </nav>

      <div className="grid items-center gap-12 xl:grid-cols-[0.88fr_1.25fr]">
        <div>
          <p className="mb-4 text-xs uppercase tracking-[0.18em] text-muted">
            closed beta · open source
          </p>
          <h1 className="text-4xl font-bold leading-[1.1] sm:text-5xl">
            Voice rooms with{" "}
            <span className="font-bold text-accent">nitro quality</span> screen
            sharing.
          </h1>
          <p className="mt-5 max-w-xl text-lg text-muted">
            Up to 1440p@60fps streams. Watch everyone at once. Up to 6 people. Free
            during beta.
          </p>

          <div className="mt-8 flex flex-wrap items-center gap-4">
            <a
              href="#updates"
              className="inline-flex items-center gap-2 rounded-md border border-accent bg-accent px-5 py-2.5 font-bold text-bg transition-colors hover:bg-transparent hover:text-accent"
              style={{ touchAction: "manipulation" }}
            >
              <span aria-hidden="true">{"▸"}</span>
              Get early access
            </a>
            <a
              href={GITHUB_URL}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-2 rounded-md border border-border px-5 py-2.5 transition-colors hover:border-blue hover:text-blue"
              style={{ touchAction: "manipulation" }}
            >
              Self-host it
            </a>
          </div>
          <div className="mt-5 flex flex-wrap gap-x-5 gap-y-2 text-sm text-muted">
            <span>macOS · Windows · Linux </span>
          </div>
        </div>

        <div className="min-w-0 xl:pl-4">
          <AppMock />
        </div>
      </div>
    </header>
  );
}
