import { useState, useCallback, useRef, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { LoadingBars } from '@shared/LoadingBars';
import { Tooltip, TooltipContent, TooltipTrigger } from '../../components/ui/tooltip';
import type {
  ShareMode,
  ShareSource,
  ShareSourceType,
  EnumerationResult,
  ShareSelection,
} from './share-types';

/* ─── Constants ─────────────────────────────────────────────────── */

const MODES: { key: ShareMode; label: string; sourceType: ShareSourceType }[] = [
  { key: 'screen_audio', label: 'Screen', sourceType: 'screen' },
  { key: 'window', label: 'Window', sourceType: 'window' },
  { key: 'audio_only', label: 'Audio Only', sourceType: 'system_audio' },
];

const ECHO_WARNING_SUBSTRING = 'echo possible';
const DEBUG_SCREEN_CAPTURE = import.meta.env.VITE_DEBUG_SCREEN_CAPTURE === 'true';
const LOG = '[share-picker]';
const PORTAL_SOURCE_ID = 'portal';

/* ─── Helpers ───────────────────────────────────────────────────── */

/** Occupied share slot info passed from ActiveRoom. */
export interface OccupiedSlots {
  videoOccupied: boolean;
  audioOccupied: boolean;
}

/**
 * Props for inline modal mode. When provided, the picker runs entirely
 * in-process — no child window, no PostMessage, no cross-window events.
 * When omitted, falls back to URL hash + Tauri event relay (Linux path).
 */
export interface SharePickerProps {
  inline?: boolean;
  enumResult?: EnumerationResult | null;
  occupied?: OccupiedSlots;
  modeScope?: 'all' | 'video_only';
  initialWithAudio?: boolean;
  onSelect?: (selection: ShareSelection) => void;
  onPortalFallback?: () => void;
  onCancel?: () => void;
}

/** Parse the picker data from the URL hash (standalone window mode). */
function parseHashData(): { enumResult: EnumerationResult | null; occupied: OccupiedSlots } | null {
  try {
    const raw = decodeURIComponent(window.location.hash.slice(1));
    if (!raw) return null;
    const data = JSON.parse(raw);
    const result: EnumerationResult | null = data.enumResult
      ? {
          sources: Array.isArray(data.enumResult.sources) ? data.enumResult.sources : [],
          warnings: Array.isArray(data.enumResult.warnings) ? data.enumResult.warnings : [],
          fallback_reason: data.enumResult.fallback_reason ?? null,
        }
      : null;
    const occupied: OccupiedSlots = {
      videoOccupied: data.occupied?.videoOccupied ?? false,
      audioOccupied: data.occupied?.audioOccupied ?? false,
    };
    return { enumResult: result, occupied };
  } catch {
    return null;
  }
}

/** Filter sources by mode. */
export function filterSourcesByMode(sources: ShareSource[], mode: ShareMode): ShareSource[] {
  const entry = MODES.find((m) => m.key === mode);
  if (!entry) return [];
  return sources.filter((s) => s.source_type === entry.sourceType);
}

function isPortalSource(source: ShareSource): boolean {
  return source.id === PORTAL_SOURCE_ID;
}

function withPortalFallbackSources(enumResult: EnumerationResult | null): ShareSource[] {
  const sources = enumResult?.sources ?? [];
  if (enumResult?.fallback_reason !== 'portal') return sources;

  const next = [...sources];
  if (!next.some((source) => source.source_type === 'screen')) {
    next.push({
      id: PORTAL_SOURCE_ID,
      name: 'Screen or display',
      source_type: 'screen',
      thumbnail: null,
      app_name: 'System picker',
    });
  }
  if (!next.some((source) => source.source_type === 'window')) {
    next.push({
      id: PORTAL_SOURCE_ID,
      name: 'Window or app',
      source_type: 'window',
      thumbnail: null,
      app_name: 'System picker',
    });
  }
  return next;
}

/** Pick the first mode that has sources, or default to screen_audio. */
function pickDefaultMode(sources: ShareSource[]): ShareMode {
  for (const m of MODES) {
    if (sources.some((s) => s.source_type === m.sourceType)) return m.key;
  }
  return 'screen_audio';
}

/** Pick the first mode that has sources and is not blocked by an occupied slot. */
export function pickInitialMode(sources: ShareSource[], occupied: OccupiedSlots): ShareMode {
  for (const allowPortal of [false, true]) {
    for (const m of MODES) {
      const blocked = m.key === 'audio_only' ? occupied.audioOccupied : occupied.videoOccupied;
      const hasSource = sources.some(
        (s) => s.source_type === m.sourceType && (allowPortal || !isPortalSource(s)),
      );
      if (!blocked && hasSource) {
        return m.key;
      }
    }
  }
  return pickDefaultMode(sources);
}

export function pickInitialVideoMode(sources: ShareSource[], occupied: OccupiedSlots): ShareMode {
  for (const m of MODES.filter((mode) => mode.key !== 'audio_only')) {
    if (!occupied.videoOccupied && sources.some((s) => s.source_type === m.sourceType)) {
      return m.key;
    }
  }
  return 'screen_audio';
}

export function defaultWithAudioForMode(mode: ShareMode, occupied: OccupiedSlots): boolean {
  if (mode === 'audio_only') return false;
  if (occupied.audioOccupied) return false;
  return mode === 'screen_audio';
}

/** Check if warnings indicate echo is possible (loopback exclusion unavailable). */
export function hasEchoWarning(warnings: string[]): boolean {
  return warnings.some((w) => w.includes(ECHO_WARNING_SUBSTRING));
}

/** Pure helper: should the portal fallback button be visible? */
export function shouldShowPortalFallback(
  fallbackReason: EnumerationResult['fallback_reason'],
): boolean {
  return fallbackReason === 'portal';
}

/* ─── Sub-components ────────────────────────────────────────────── */

function ModeTab({
  mode,
  label,
  active,
  index,
  disabled,
  onSelect,
}: {
  mode: ShareMode;
  label: string;
  active: boolean;
  index: number;
  disabled?: boolean;
  onSelect: (m: ShareMode) => void;
}) {
  return (
    <button
      role="tab"
      id={`tab-${mode}`}
      aria-selected={active}
      aria-controls={`tabpanel-${mode}`}
      aria-disabled={disabled}
      tabIndex={active ? 0 : -1}
      disabled={disabled}
      className={[
        'px-3 py-1.5 font-mono text-sm border-b-2 transition-colors',
        'focus:outline focus:outline-2 focus:outline-wavis-accent',
        disabled
          ? 'border-transparent text-wavis-text-secondary opacity-40 cursor-not-allowed'
          : active
            ? 'border-wavis-accent text-wavis-accent'
            : 'border-transparent text-wavis-text-secondary hover:text-wavis-text',
      ].join(' ')}
      onClick={() => {
        if (!disabled) onSelect(mode);
      }}
      onKeyDown={(e) => {
        if (disabled) return;
        if (e.key === 'ArrowRight') {
          const next = MODES[(index + 1) % MODES.length];
          onSelect(next.key);
          document.getElementById(`tab-${next.key}`)?.focus();
        } else if (e.key === 'ArrowLeft') {
          const prev = MODES[(index - 1 + MODES.length) % MODES.length];
          onSelect(prev.key);
          document.getElementById(`tab-${prev.key}`)?.focus();
        }
      }}
    >
      {label}
      {disabled ? ' (active)' : ''}
    </button>
  );
}

function SourceItem({
  source,
  selected,
  onSelect,
  isVisual,
  resolvedThumbnail,
  isThumbnailLoading,
  showEchoWarning,
  onThumbnailError,
}: {
  source: ShareSource;
  selected: boolean;
  onSelect: (s: ShareSource) => void;
  isVisual: boolean;
  resolvedThumbnail?: string;
  isThumbnailLoading?: boolean;
  showEchoWarning?: boolean;
  onThumbnailError?: () => void;
}) {
  const thumb = resolvedThumbnail ?? source.thumbnail;
  return (
    <div
      role="option"
      aria-selected={selected}
      tabIndex={-1}
      className={[
        'flex items-center gap-3 p-2 cursor-pointer transition-colors',
        'focus:outline focus:outline-2 focus:outline-wavis-accent',
        selected
          ? 'border border-wavis-accent bg-wavis-panel'
          : 'border border-transparent hover:border-wavis-text-secondary',
      ].join(' ')}
      onClick={() => onSelect(source)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect(source);
        }
      }}
    >
      {isVisual && (
        <div className="w-24 h-16 shrink-0 bg-wavis-panel border border-wavis-text-secondary flex items-center justify-center overflow-hidden">
          {thumb ? (
            <img
              src={`data:image/jpeg;base64,${thumb}`}
              alt={source.name}
              className="w-full h-full object-cover"
              onError={onThumbnailError}
            />
          ) : isThumbnailLoading ? (
            <LoadingBars
              className="flex items-center justify-center"
              barClassName="w-1 bg-wavis-purple"
              ariaLabel="Loading thumbnail"
            />
          ) : (
            <span className="text-wavis-text-secondary text-[0.625rem] text-center px-1 truncate">
              {source.name}
            </span>
          )}
        </div>
      )}
      <div className="min-w-0 flex-1">
        <div className="text-sm text-wavis-text truncate flex items-center gap-1">
          {source.name}
          {showEchoWarning && (
            <span
              className="text-wavis-warn shrink-0"
              aria-label="System audio may include your own voice — echo possible"
              title="System audio may include your own voice — echo possible"
            >
              ⚠
            </span>
          )}
        </div>
        {source.app_name && (
          <div className="text-[0.625rem] text-wavis-text-secondary truncate">
            {source.app_name}
          </div>
        )}
      </div>
      {selected && <span className="text-wavis-accent text-sm shrink-0">▸</span>}
    </div>
  );
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function SharePicker(props: SharePickerProps) {
  // ── Mode detection: inline modal (props) vs standalone window (hash) ──
  const isInline = props.inline === true || props.enumResult !== undefined;

  /* ── State ── */
  const [parsed, setParsed] = useState(() => (isInline ? null : parseHashData()));

  const enumResult = isInline ? (props.enumResult ?? null) : (parsed?.enumResult ?? null);

  const occupied: OccupiedSlots = isInline
    ? (props.occupied ?? { videoOccupied: false, audioOccupied: false })
    : (parsed?.occupied ?? { videoOccupied: false, audioOccupied: false });

  const initPickerSources = withPortalFallbackSources(enumResult);
  const modeScope = props.modeScope ?? 'all';
  const initialMode =
    modeScope === 'video_only'
      ? pickInitialVideoMode(initPickerSources, occupied)
      : pickInitialMode(initPickerSources, occupied);

  const [activeMode, setActiveMode] = useState<ShareMode>(() =>
    initPickerSources.length > 0 ? initialMode : 'screen_audio',
  );
  const [selectedSource, setSelectedSource] = useState<ShareSource | null>(null);
  const [withAudio, setWithAudio] = useState<boolean>(
    () =>
      props.initialWithAudio ??
      defaultWithAudioForMode(
        initPickerSources.length > 0 ? initialMode : 'screen_audio',
        occupied,
      ),
  );
  // TODO(persistence): resets on each picker open; follow-up is to persist per-app in settings store keyed by app_name.
  const [compatibilityMode, setCompatibilityMode] = useState(false);

  const [thumbnails, setThumbnails] = useState<Record<string, string>>({});
  const [thumbnailsLoading, setThumbnailsLoading] = useState(new Set<string>());

  const listboxRef = useRef<HTMLDivElement>(null);
  const activeIndexRef = useRef<number>(-1);

  /* ── Standalone source loading ── */
  useEffect(() => {
    if (isInline || enumResult !== null) return;
    let cancelled = false;

    invoke<EnumerationResult>('list_share_sources')
      .then((result) => {
        if (!cancelled) {
          setParsed((current) => ({
            enumResult: result,
            occupied: current?.occupied ?? { videoOccupied: false, audioOccupied: false },
          }));
        }
      })
      .catch((err: unknown) => {
        if (DEBUG_SCREEN_CAPTURE) console.error(LOG, 'source enumeration failed', err);
        if (!cancelled) {
          setParsed((current) => ({
            enumResult: { sources: [], warnings: [String(err)], fallback_reason: null },
            occupied: current?.occupied ?? { videoOccupied: false, audioOccupied: false },
          }));
        }
      });

    return () => {
      cancelled = true;
    };
  }, [enumResult, isInline]);

  /* ── Lazy thumbnail loading ── */
  useEffect(() => {
    if (!enumResult) return;
    let cancelled = false;

    const visualSources = enumResult.sources.filter(
      (s) => !isPortalSource(s) && (s.source_type === 'screen' || s.source_type === 'window'),
    );

    if (visualSources.length > 0) {
      setThumbnailsLoading(new Set(visualSources.map((s) => s.id)));
    }

    for (const source of visualSources) {
      if (DEBUG_SCREEN_CAPTURE) console.log(LOG, 'fetching thumbnail for', source.id, source.name);
      invoke<string | null>('fetch_source_thumbnail', { sourceId: source.id })
        .then((thumb) => {
          if (DEBUG_SCREEN_CAPTURE)
            console.log(
              LOG,
              'thumbnail result for',
              source.id,
              thumb ? `${thumb.length} bytes` : 'null',
            );
          if (!cancelled) {
            setThumbnailsLoading((prev) => {
              const next = new Set(prev);
              next.delete(source.id);
              return next;
            });
            if (thumb) setThumbnails((prev) => ({ ...prev, [source.id]: thumb }));
          }
        })
        .catch((err: unknown) => {
          if (DEBUG_SCREEN_CAPTURE)
            console.error(LOG, 'thumbnail fetch failed for', source.id, err);
          if (!cancelled) {
            setThumbnailsLoading((prev) => {
              const next = new Set(prev);
              next.delete(source.id);
              return next;
            });
          }
        });
    }

    return () => {
      cancelled = true;
    };
  }, [enumResult]);

  const isWindowsPlatform = /Windows NT/i.test(navigator.userAgent || '');

  /* ── Derived ── */
  const sources = withPortalFallbackSources(enumResult);
  const warnings = enumResult?.warnings ?? [];
  const filteredSources = filterSourcesByMode(sources, activeMode);
  const isVisualMode = activeMode !== 'audio_only';
  const showAudioCheckbox = activeMode !== 'audio_only';
  // Disable system audio checkbox when an audio-only share is already occupying the audio device.
  const audioCheckboxDisabled = occupied.audioOccupied || modeScope === 'video_only';
  const canShare = selectedSource !== null;
  const showFallback =
    enumResult !== null &&
    shouldShowPortalFallback(enumResult.fallback_reason) &&
    filteredSources.length === 0;
  const isEmpty = filteredSources.length === 0 && !showFallback;
  const echoWarningActive = hasEchoWarning(warnings);

  useEffect(() => {
    if (occupied.audioOccupied && withAudio) {
      setWithAudio(false);
    }
  }, [occupied.audioOccupied, withAudio]);

  /** Check if a mode tab should be disabled due to occupied slot. */
  const isModeDisabled = (mode: ShareMode): boolean => {
    if (modeScope === 'video_only' && mode === 'audio_only') return true;
    if (mode === 'audio_only') return occupied.audioOccupied;
    return occupied.videoOccupied;
  };

  /* ── Mode change resets selection and audio checkbox ── */
  const handleModeChange = useCallback(
    (mode: ShareMode) => {
      setActiveMode(mode);
      setSelectedSource(null);
      activeIndexRef.current = -1;
      setWithAudio((current) => {
        if (modeScope === 'video_only') {
          return props.initialWithAudio ?? current;
        }
        if (mode === 'audio_only' || occupied.audioOccupied) {
          return false;
        }
        if (mode === 'screen_audio') {
          return true;
        }
        return current;
      });
    },
    [modeScope, occupied.audioOccupied, props.initialWithAudio],
  );

  /* ── Source selection ── */
  const handleSourceSelect = useCallback(
    (source: ShareSource) => {
      setSelectedSource(source);
      activeIndexRef.current = filteredSources.findIndex((s) => s.id === source.id);
    },
    [filteredSources],
  );

  /* ── Arrow key navigation in source list ── */
  const handleListKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (filteredSources.length === 0) return;

      let nextIndex = activeIndexRef.current;

      if (e.key === 'ArrowDown') {
        e.preventDefault();
        nextIndex = Math.min(activeIndexRef.current + 1, filteredSources.length - 1);
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        nextIndex = Math.max(activeIndexRef.current - 1, 0);
      } else {
        return;
      }

      activeIndexRef.current = nextIndex;
      setSelectedSource(filteredSources[nextIndex]);

      const listbox = listboxRef.current;
      if (listbox) {
        const options = listbox.querySelectorAll<HTMLElement>('[role="option"]');
        options[nextIndex]?.focus();
      }
    },
    [filteredSources],
  );

  /* ── Share action ── */
  const handleShare = useCallback(async () => {
    if (!selectedSource) return;

    const selection: ShareSelection = {
      mode: activeMode,
      sourceId: selectedSource.id,
      sourceName: isPortalSource(selectedSource) ? 'System picker' : selectedSource.name,
      withAudio: activeMode === 'audio_only' ? false : withAudio,
      sourceKind: selectedSource.source_type === 'window' ? 'window' : 'screen',
      compatibilityMode,
    };

    if (isInline) {
      // In-app modal: call parent callback directly — no cross-window events.
      props.onSelect?.(selection);
    } else {
      // Standalone window (Linux): relay through Rust + close window.
      await invoke('share_picker_select', { selection });
      await getCurrentWindow().close();
    }
  }, [selectedSource, activeMode, withAudio, compatibilityMode, isInline, props]);

  /* ── Cancel action ── */
  const handleCancel = useCallback(async () => {
    if (isInline) {
      props.onCancel?.();
    } else {
      await invoke('share_picker_cancel');
      await getCurrentWindow().close();
    }
  }, [isInline, props]);

  /* ── Portal fallback action ── */
  const handlePortalFallback = useCallback(async () => {
    if (isInline) {
      props.onPortalFallback?.();
    } else {
      await invoke('share_picker_use_portal');
      await getCurrentWindow().close();
    }
  }, [isInline, props]);

  /* ── Escape key ── */
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        handleCancel();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [handleCancel]);

  /* ── Picker content (shared between inline and standalone) ── */
  const pickerContent = (
    <div
      className={[
        'flex flex-col bg-wavis-bg font-mono text-wavis-text select-none',
        isInline ? 'h-full' : 'min-h-screen',
      ].join(' ')}
    >
      {/* ── Header ── */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-wavis-text-secondary">
        <span className="text-sm text-wavis-accent">▲ Share Picker</span>
        <button
          onClick={handleCancel}
          className="text-wavis-danger hover:opacity-70 text-sm focus:outline focus:outline-2 focus:outline-wavis-accent"
          aria-label="Close share picker"
        >
          [x]
        </button>
      </div>

      {/* ── Mode Tabs ── */}
      <div
        role="tablist"
        aria-label="Share mode"
        className="flex border-b border-wavis-text-secondary px-4"
      >
        {MODES.map((m, i) => (
          <ModeTab
            key={m.key}
            mode={m.key}
            label={m.label}
            active={activeMode === m.key}
            index={i}
            disabled={isModeDisabled(m.key)}
            onSelect={handleModeChange}
          />
        ))}
      </div>

      {/* ── Tab Panel ── */}
      <div
        role="tabpanel"
        id={`tabpanel-${activeMode}`}
        aria-labelledby={`tab-${activeMode}`}
        className="flex-1 overflow-y-auto px-4 py-3 @container"
      >
        {enumResult === null ? (
          <div className="flex flex-col items-center justify-center h-full gap-3 text-wavis-text-secondary">
            <span className="text-sm">Loading sources...</span>
          </div>
        ) : isEmpty ? (
          <div className="flex flex-col items-center justify-center h-full gap-3 text-wavis-text-secondary">
            <span className="text-sm">No shareable sources found</span>
          </div>
        ) : showFallback ? (
          <div className="flex flex-col items-center justify-center h-full gap-3">
            <span className="text-sm text-wavis-warn">⚠ Direct access unavailable</span>
            <button
              onClick={handlePortalFallback}
              className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1 focus:outline focus:outline-2 focus:outline-wavis-accent"
            >
              Use system picker
            </button>
          </div>
        ) : (
          <div
            ref={listboxRef}
            role="listbox"
            aria-label="Available sources"
            aria-activedescendant={selectedSource ? `source-${selectedSource.id}` : undefined}
            tabIndex={0}
            onKeyDown={handleListKeyDown}
            className={[
              'grid',
              isVisualMode
                ? 'grid-cols-1 gap-2 @[480px]:grid-cols-2'
                : 'grid-cols-1 divide-y divide-wavis-text-secondary/20',
              'focus:outline focus:outline-2 focus:outline-wavis-accent',
            ].join(' ')}
          >
            {filteredSources.map((source) => (
              <div key={source.id} id={`source-${source.id}`}>
                <SourceItem
                  source={source}
                  selected={selectedSource?.id === source.id}
                  onSelect={handleSourceSelect}
                  isVisual={isVisualMode}
                  resolvedThumbnail={thumbnails[source.id]}
                  isThumbnailLoading={thumbnailsLoading.has(source.id)}
                  showEchoWarning={echoWarningActive && source.source_type === 'system_audio'}
                  onThumbnailError={() =>
                    setThumbnails((prev) => {
                      const next = { ...prev };
                      delete next[source.id];
                      return next;
                    })
                  }
                />
              </div>
            ))}
          </div>
        )}
      </div>

      {/* ── Footer ── */}
      <div className="border-t border-wavis-text-secondary px-4 py-3 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-col items-start gap-2 sm:flex-row sm:items-center sm:gap-3">
          {showAudioCheckbox && (
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <input
                type="checkbox"
                checked={withAudio}
                disabled={audioCheckboxDisabled}
                onChange={(e) => setWithAudio(e.target.checked)}
                className="accent-wavis-accent focus:outline focus:outline-2 focus:outline-wavis-accent disabled:opacity-40"
              />
              <span
                className={audioCheckboxDisabled ? 'text-wavis-text-secondary' : 'text-wavis-text'}
              >
                System audio
              </span>
            </label>
          )}
          {activeMode === 'window' && isWindowsPlatform && (
            <div className="flex items-center gap-1 text-sm select-none">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={compatibilityMode}
                  onChange={(e) => setCompatibilityMode(e.target.checked)}
                  className="accent-wavis-accent"
                />
                <span className="text-wavis-text-secondary">Game compatibility mode</span>
              </label>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    aria-label="Game compatibility mode help"
                    className="inline-flex h-5 w-5 items-center justify-center border border-wavis-text-secondary text-[0.6875rem] text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent focus:outline focus:outline-2 focus:outline-wavis-accent"
                  >
                    ?
                  </button>
                </TooltipTrigger>
                <TooltipContent
                  side="top"
                  sideOffset={6}
                  className="max-w-72 border border-wavis-text-secondary bg-wavis-panel text-wavis-text shadow-lg"
                >
                  Use this for games or apps that show a black screen, missing cursor, or fail to
                  capture with the default method.
                </TooltipContent>
              </Tooltip>
            </div>
          )}
        </div>
        <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center sm:gap-3">
          <button
            onClick={handleCancel}
            className="border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg transition-colors px-4 py-1 focus:outline focus:outline-2 focus:outline-wavis-accent"
          >
            Cancel
          </button>
          <button
            onClick={handleShare}
            disabled={!canShare}
            className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1 disabled:opacity-40 disabled:cursor-not-allowed focus:outline focus:outline-2 focus:outline-wavis-accent"
          >
            Share
          </button>
        </div>
      </div>
    </div>
  );

  // ── Inline modal: wrap in a fixed overlay with backdrop ──
  if (isInline) {
    return (
      <div
        className="fixed inset-0 z-50 flex items-center justify-center bg-wavis-overlay-base/60"
        onClick={(e) => {
          // Click on backdrop (not on the picker itself) → cancel
          if (e.target === e.currentTarget) handleCancel();
        }}
        onKeyDown={(e) => {
          if (e.key === 'Escape') handleCancel();
        }}
      >
        <div className="w-[640px] max-w-[95vw] h-[480px] max-h-[90vh] border border-wavis-text-secondary shadow-lg">
          {pickerContent}
        </div>
      </div>
    );
  }

  // ── Standalone window: render full-screen ──
  return pickerContent;
}
