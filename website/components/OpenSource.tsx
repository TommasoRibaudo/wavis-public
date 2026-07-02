import { GITHUB_URL, LICENSE_URL, RELEASES_URL } from "../lib/config";

export function OpenSource() {
  return (
    <section className="mx-auto w-full max-w-5xl px-6 pb-20 pt-8 sm:pb-24 sm:pt-10">
      <div className="rounded-md border border-border bg-bg p-8 shadow-2xl shadow-black/15 sm:p-10">
        <p className="mb-2 text-xs uppercase tracking-widest text-muted">
          open source
        </p>
        <h2 className="text-2xl font-bold sm:text-3xl">
          Read the code, build it yourself
        </h2>
        <p className="mt-3 max-w-2xl text-muted">
          Wavis is developed in the open. Every build ships through GitHub
          Releases, and the source is licensed under MIT, so you can audit it,
          fork it, or run your own.
        </p>

        <div className="mt-6 flex flex-wrap gap-3">
          <a
            href={GITHUB_URL}
            className="rounded-md border border-border px-4 py-2 text-sm transition-colors hover:border-blue hover:text-blue"
            style={{ touchAction: "manipulation" }}
          >
            TommasoRibaudo/wavis-public
          </a>
          <a
            href={RELEASES_URL}
            className="rounded-md border border-border px-4 py-2 text-sm transition-colors hover:border-accent hover:text-accent"
            style={{ touchAction: "manipulation" }}
          >
            GitHub Releases
          </a>
          <a
            href={LICENSE_URL}
            className="rounded-md border border-border px-4 py-2 text-sm transition-colors hover:border-purple hover:text-purple"
            style={{ touchAction: "manipulation" }}
          >
            MIT license
          </a>
        </div>
      </div>
    </section>
  );
}
