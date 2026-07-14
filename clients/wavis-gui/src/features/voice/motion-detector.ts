/**
 * Pure ring-buffer-backed motion detector for auto Detail↔Motion profile switching.
 * No DOM access. Thresholds from tasks-addendum.md §A1.
 * Consumed by voice-room.ts unconditionally (no user gate — see A1.2/A1.5).
 */
import type { ShareProfileId } from './livekit-media';

export interface MotionSample {
  timestampMs: number;
  /** Frame-over-frame absolute luma change ratio 0..1. */
  changedAreaRatio: number;
}

export interface MotionDetectorConfig {
  /** Fraction of frame that must show change to be "above threshold". Default 0.40. */
  switchInAreaThreshold: number;
  /** Hysteresis threshold for switch-out. Default 0.10. */
  switchOutAreaThreshold: number;
  /** How long the condition must persist before Detail→Motion switch (ms). Default 1000. */
  switchInDwellMs: number;
  /** How long the condition must persist before Motion→Detail switch (ms). Default 10000. */
  switchOutDwellMs: number;
  /** Rolling window for computing average changedAreaRatio (ms). Default 1000. */
  sampleWindowMs: number;
  /** Expected sample rate in Hz. Used to size the ring buffer. Default 4. */
  sampleRateHz: number;
}

export const DEFAULT_MOTION_DETECTOR_CONFIG: MotionDetectorConfig = {
  switchInAreaThreshold: 0.4,
  switchOutAreaThreshold: 0.1,
  switchInDwellMs: 1_000,
  switchOutDwellMs: 5_000,
  sampleWindowMs: 1_000,
  sampleRateHz: 4,
};

export class MotionDetector {
  private readonly config: MotionDetectorConfig;

  // Fixed-capacity ring buffer: capacity covers sampleWindowMs at sampleRateHz with 2× headroom.
  private readonly capacity: number;
  private readonly buf: MotionSample[];
  private head = 0;
  private bufSize = 0;

  private _recommendation: ShareProfileId = 'detail';
  private _lastSwitchReason: 'auto_switch_in' | 'auto_switch_out' | null = null;

  // Dwell timers: timestamp when the current continuous condition first became true.
  private aboveThresholdSinceMs: number | null = null;
  private belowThresholdSinceMs: number | null = null;

  constructor(config: Partial<MotionDetectorConfig> = {}) {
    this.config = { ...DEFAULT_MOTION_DETECTOR_CONFIG, ...config };
    this.capacity = Math.max(
      8,
      Math.ceil(this.config.sampleRateHz * (this.config.sampleWindowMs / 1000)) * 2,
    );
    this.buf = new Array<MotionSample>(this.capacity);
  }

  push(sample: MotionSample): void {
    this.buf[this.head] = sample;
    this.head = (this.head + 1) % this.capacity;
    if (this.bufSize < this.capacity) this.bufSize++;

    const avgRatio = this._windowAvg(sample.timestampMs);

    if (this._recommendation === 'detail') {
      this._tickSwitchIn(sample.timestampMs, avgRatio);
    } else {
      this._tickSwitchOut(sample.timestampMs, avgRatio);
    }
  }

  currentRecommendation(): ShareProfileId {
    return this._recommendation;
  }

  lastSwitchReason(): 'auto_switch_in' | 'auto_switch_out' | null {
    return this._lastSwitchReason;
  }

  // ── Private helpers ──────────────────────────────────────────────────────────

  private _windowAvg(nowMs: number): number {
    const cutoff = nowMs - this.config.sampleWindowMs;
    let sum = 0;
    let count = 0;
    const start = (this.head - this.bufSize + this.capacity) % this.capacity;
    for (let i = 0; i < this.bufSize; i++) {
      const s = this.buf[(start + i) % this.capacity];
      if (s.timestampMs > cutoff) {
        sum += s.changedAreaRatio;
        count++;
      }
    }
    return count > 0 ? sum / count : 0;
  }

  private _tickSwitchIn(nowMs: number, avgRatio: number): void {
    if (avgRatio >= this.config.switchInAreaThreshold) {
      if (this.aboveThresholdSinceMs === null) this.aboveThresholdSinceMs = nowMs;
      this.belowThresholdSinceMs = null;
      if (nowMs - this.aboveThresholdSinceMs >= this.config.switchInDwellMs) {
        this._recommendation = 'motion';
        this._lastSwitchReason = 'auto_switch_in';
        this.aboveThresholdSinceMs = null;
      }
    } else {
      this.aboveThresholdSinceMs = null;
    }
  }

  private _tickSwitchOut(nowMs: number, avgRatio: number): void {
    if (avgRatio < this.config.switchOutAreaThreshold) {
      if (this.belowThresholdSinceMs === null) this.belowThresholdSinceMs = nowMs;
      this.aboveThresholdSinceMs = null;
      if (nowMs - this.belowThresholdSinceMs >= this.config.switchOutDwellMs) {
        this._recommendation = 'detail';
        this._lastSwitchReason = 'auto_switch_out';
        this.belowThresholdSinceMs = null;
      }
    } else {
      this.belowThresholdSinceMs = null;
    }
  }
}
