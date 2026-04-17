import type { AudioDriverState } from './useAudioDriverInstall';

interface AudioDriverInstallPromptProps {
  state: AudioDriverState;
  installError: string | null;
  onInstall: () => void;
  onSkip: () => void;
}

export function AudioDriverInstallPrompt({
  state,
  installError,
  onInstall,
  onSkip,
}: AudioDriverInstallPromptProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-wavis-overlay-base/60">
      <div className="w-[480px] max-w-[95vw] bg-wavis-bg border border-wavis-text-secondary font-mono text-wavis-text p-6 flex flex-col gap-4 shadow-lg">
        <div className="text-sm text-wavis-accent">▲ Audio Driver Required</div>

        {(state === 'not_installed' || state === 'checking') && (
          <>
            <p className="text-sm text-wavis-text leading-relaxed">
              Wavis needs <strong>BlackHole 2ch</strong> to prevent echo during screen share.
              It's a free, open-source virtual audio device — click below to open the download page.
              <span className="text-wavis-text-secondary"> After installing, restart Wavis.</span>
            </p>
            <div className="flex gap-2 justify-end">
              <button
                className="text-sm text-wavis-text-secondary hover:text-wavis-text px-3 py-1 border border-wavis-text-secondary hover:border-wavis-text focus:outline focus:outline-2 focus:outline-wavis-accent"
                onClick={onSkip}
              >
                Skip for Now
              </button>
              <button
                className="text-sm text-wavis-accent px-3 py-1 border border-wavis-accent hover:opacity-70 focus:outline focus:outline-2 focus:outline-wavis-accent"
                onClick={onInstall}
              >
                Open Download Page
              </button>
            </div>
          </>
        )}

        {state === 'browser_opened' && (
          <>
            <p className="text-sm text-wavis-text leading-relaxed">
              The BlackHole download page has been opened in your browser.
              Install <strong>BlackHole 2ch</strong>, then restart Wavis to enable echo-free sharing.
            </p>
            <div className="flex gap-2 justify-end">
              <button
                className="text-sm text-wavis-text-secondary hover:text-wavis-text px-3 py-1 border border-wavis-text-secondary hover:border-wavis-text focus:outline focus:outline-2 focus:outline-wavis-accent"
                onClick={onSkip}
              >
                Share Anyway
              </button>
            </div>
          </>
        )}

        {state === 'install_failed' && (
          <>
            <p className="text-sm text-wavis-text leading-relaxed">
              Something went wrong. You can still share audio without BlackHole.
            </p>
            {installError && (
              <p className="text-xs text-wavis-danger font-mono break-all">{installError}</p>
            )}
            <div className="flex gap-2 justify-end">
              <button
                className="text-sm text-wavis-text-secondary hover:text-wavis-text px-3 py-1 border border-wavis-text-secondary hover:border-wavis-text focus:outline focus:outline-2 focus:outline-wavis-accent"
                onClick={onSkip}
              >
                Share Anyway
              </button>
              <button
                className="text-sm text-wavis-accent px-3 py-1 border border-wavis-accent hover:opacity-70 focus:outline focus:outline-2 focus:outline-wavis-accent"
                onClick={onInstall}
              >
                Try Again
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
