"use client";

import { useId, useRef, useState } from "react";

const FORM_ACTION = process.env.NEXT_PUBLIC_SUBSCRIBE_FORM_ACTION ?? "";
const ENTRY_ID = process.env.NEXT_PUBLIC_SUBSCRIBE_ENTRY_ID ?? "";

// Pragmatic email check — rejects obvious typos without blocking valid edge cases.
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

type Status = "idle" | "submitting" | "done" | "error";

export function MailingList() {
  const inputId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const [status, setStatus] = useState<Status>("idle");
  const [message, setMessage] = useState("");

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const email = inputRef.current?.value.trim() ?? "";

    if (!EMAIL_RE.test(email)) {
      setStatus("error");
      setMessage("Enter a valid email address.");
      inputRef.current?.focus();
      return;
    }

    setStatus("submitting");
    setMessage("");

    // Google Forms returns an opaque (no-cors) response, so success can't be
    // read from it — show an optimistic confirmation after the request settles.
    try {
      if (FORM_ACTION && ENTRY_ID) {
        const body = new URLSearchParams({ [ENTRY_ID]: email });
        await fetch(FORM_ACTION, { method: "POST", mode: "no-cors", body });
      } else if (process.env.NODE_ENV !== "production") {
        console.warn(
          "Mailing list endpoint not configured. Set NEXT_PUBLIC_SUBSCRIBE_FORM_ACTION and NEXT_PUBLIC_SUBSCRIBE_ENTRY_ID.",
        );
      }
      setStatus("done");
      setMessage("You’re on the list. We’ll email you when new builds are ready.");
    } catch {
      setStatus("error");
      setMessage("That didn’t go through. Try again in a moment.");
    }
  }

  return (
    <section
      id="updates"
      className="mx-auto w-full max-w-5xl scroll-mt-8 px-6 py-20 sm:py-24"
    >
      <div className="rounded-md border border-border bg-panel p-8 sm:p-10">
        <p className="mb-2 text-xs uppercase tracking-widest text-muted">
          mailing list
        </p>
        <h2 className="text-2xl font-bold sm:text-3xl">Get release updates</h2>
        <p className="mt-3 max-w-xl text-muted">
          Occasional notes when new Wavis builds are ready. No other email.
        </p>

        {status === "done" ? (
          <p
            className="mt-6 flex items-center gap-2 text-accent"
            role="status"
            aria-live="polite"
          >
            <span aria-hidden="true">▸</span>
            {message}
          </p>
        ) : (
          <form
            onSubmit={onSubmit}
            noValidate
            className="mt-6 flex flex-col gap-3 sm:max-w-md"
          >
            <label htmlFor={inputId} className="sr-only">
              Email address
            </label>
            <div className="flex flex-col gap-3 sm:flex-row">
              <input
                ref={inputRef}
                id={inputId}
                name="email"
                type="email"
                inputMode="email"
                autoComplete="email"
                spellCheck={false}
                required
                placeholder="you@example.com"
                aria-invalid={status === "error"}
                aria-describedby={status === "error" ? `${inputId}-error` : undefined}
                disabled={status === "submitting"}
                className="flex-1 rounded-md border border-border bg-crust px-3 py-2.5 text-text placeholder:text-muted disabled:opacity-60"
                style={{ touchAction: "manipulation" }}
              />
              <button
                type="submit"
                disabled={status === "submitting"}
                className="inline-flex items-center justify-center gap-2 rounded-md border border-accent bg-accent px-5 py-2.5 font-bold text-bg transition-colors hover:bg-transparent hover:text-accent disabled:opacity-60"
                style={{ touchAction: "manipulation" }}
              >
                {status === "submitting" ? "subscribing…" : "/subscribe"}
              </button>
            </div>
            {status === "error" && (
              <p
                id={`${inputId}-error`}
                role="alert"
                className="text-sm text-danger"
              >
                {message}
              </p>
            )}
          </form>
        )}
      </div>
    </section>
  );
}
