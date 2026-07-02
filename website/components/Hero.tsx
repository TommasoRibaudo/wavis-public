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

      <div className="grid items-center gap-12 lg:grid-cols-[0.95fr_1.15fr]">
        <div>
          <p className="mb-4 text-xs uppercase tracking-[0.18em] text-muted">
            native desktop · open source
          </p>
          <h1 className="text-4xl font-bold leading-[1.1] sm:text-5xl">
            Native voice and screen sharing.
          </h1>
          <p className="mt-5 max-w-xl text-lg text-muted">
            A lightweight desktop app for calls, camera, sharp screen sharing,
            and audio-only streams.
          </p>

          <div className="mt-8 flex flex-wrap items-center gap-4">
            <PrimaryDownloadButton showMeta={false} />
            <a
              href={GITHUB_URL}
              className="inline-flex items-center gap-2 rounded-md border border-border px-5 py-2.5 transition-colors hover:border-blue hover:text-blue"
              style={{ touchAction: "manipulation" }}
            >
              View on GitHub
            </a>
          </div>
          <div className="mt-5 flex flex-wrap gap-x-5 gap-y-2 text-sm text-muted">
            <span>macOS · Windows · Linux</span>
            <a
              href="#updates"
              className="underline-offset-4 hover:text-text hover:underline"
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
