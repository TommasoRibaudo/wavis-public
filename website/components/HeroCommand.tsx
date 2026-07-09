"use client";

import { useEffect, useState } from "react";

const CMD = "/join ";
const CHANNEL = "#general";
const TOTAL = CMD.length + CHANNEL.length;

// Types the command once on load, then leaves a blinking caret.
// Under prefers-reduced-motion the full command is shown immediately.
export function HeroCommand() {
  const [shown, setShown] = useState(TOTAL);

  useEffect(() => {
    const reduce = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    if (reduce) return;

    setShown(0);
    let i = 0;
    const id = window.setInterval(() => {
      i += 1;
      setShown(i);
      if (i >= TOTAL) window.clearInterval(id);
    }, 90);
    return () => window.clearInterval(id);
  }, []);

  const cmdShown = CMD.slice(0, Math.min(shown, CMD.length));
  const channelShown = CHANNEL.slice(0, Math.max(0, shown - CMD.length));

  return (
    <div className="flex items-center gap-2 text-sm">
      <span className="text-accent" aria-hidden="true">
        ❯
      </span>
      <span aria-hidden="true">
        <span className="text-blue">{cmdShown}</span>
        <span className="text-purple">{channelShown}</span>
      </span>
      <span
        aria-hidden="true"
        className="inline-block h-[1.05em] w-[0.5em] bg-text align-middle"
        style={{ animation: "wavis-caret 1.05s step-end infinite" }}
      />
      <span className="sr-only">Example command: join the general channel</span>
    </div>
  );
}
