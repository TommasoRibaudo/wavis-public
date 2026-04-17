/**
 * Wavis Share Picker Types
 *
 * Shared type definitions for the custom share picker, share
 * indicator, and voice-room share orchestration. These mirror
 * the Rust-side ShareSource / EnumerationResult structs from
 * share_sources.rs and are used across SharePicker.tsx,
 * ShareIndicator.tsx, and voice-room.ts.
 */

/* ─── Share Mode ────────────────────────────────────────────────── */

/** Capture mode selected by the user in the share picker. */
export type ShareMode = 'screen_audio' | 'window' | 'audio_only';

/* ─── Source Types ──────────────────────────────────────────────── */

/** Discriminant matching the Rust `ShareSourceType` enum (snake_case serde). */
export type ShareSourceType = 'screen' | 'window' | 'system_audio';

/** A single shareable source returned by `list_share_sources`. */
export interface ShareSource {
  /** Opaque platform identifier (PipeWire node ID, PulseAudio sink name). */
  id: string;
  /** Human-readable name ("Built-in Display", "Firefox", "System Audio"). */
  name: string;
  /** Source category. */
  source_type: ShareSourceType;
  /** Base64-encoded PNG thumbnail (Screen/Window only, null for SystemAudio). */
  thumbnail: string | null;
  /** Application name (Window sources only). */
  app_name: string | null;
}

/* ─── Fallback Reason ───────────────────────────────────────────── */

/** Why enumeration returned zero sources — drives fallback routing. */
export type FallbackReason = 'portal' | 'get_display_media';

/* ─── Enumeration Result ────────────────────────────────────────── */

/** Result from the `list_share_sources` Tauri command. */
export interface EnumerationResult {
  /** Discovered shareable sources. */
  sources: ShareSource[];
  /** Non-fatal warnings (e.g. "PulseAudio unavailable"). */
  warnings: string[];
  /** When present, indicates why enumeration returned zero sources and which
   *  fallback path the frontend should use. null = custom picker is fully functional. */
  fallback_reason: FallbackReason | null;
}

/* ─── Audio Share Start Result ───────────────────────────────────── */

/** Result from the `audio_share_start` Tauri command. */
export interface AudioShareStartResult {
  /** Whether audio isolation is active for this share session. */
  loopback_exclusion_available: boolean;
  /** CoreAudio UID for the real output device when virtual-device routing is active. */
  real_output_device_id?: string | null;
  /** Human-readable name of the real output device (e.g. "MacBook Pro Speakers").
   * Used as a label-based fallback when the CoreAudio UID doesn't match any browser deviceId. */
  real_output_device_name?: string | null;
  /** When true, the capture path cannot exclude room audio (e.g. macOS virtual-device
   * path where setSinkId is unavailable). The caller must mute local playback to
   * prevent the viewer from hearing their own voice in loopback. */
  requires_mute_for_echo_prevention?: boolean;
}

/* ─── Share Selection ───────────────────────────────────────────── */

/** Payload emitted by the share picker when the user confirms a selection. */
export interface ShareSelection {
  /** Chosen capture mode. */
  mode: ShareMode;
  /** Opaque source ID to pass to the capture command. */
  sourceId: string;
  /** Human-readable source name (for the share indicator). */
  sourceName: string;
  /** Whether system audio capture is enabled alongside video. */
  withAudio: boolean;
}
