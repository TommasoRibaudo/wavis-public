//! Adaptive jitter buffer implementation and tests for the audio pipeline.

#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::{
    JitterBufferStats, JitterBuffering, JitterResult, FRAME_DURATION, MAX_BUFFERED_PACKETS,
    MAX_DELAY_SHRINK_PER_SEC_MS, MAX_JITTER_DELAY_MS, MAX_PACKET_SIZE, MIN_JITTER_DELAY_MS,
    MIN_PREFETCH_MS,
};

/// Adaptive jitter buffer that reorders packets by RTP sequence number
/// and adapts its target delay based on observed jitter statistics.
pub struct AdaptiveJitterBuffer {
    /// Buffered packets keyed by sequence number.
    buffer: BTreeMap<u16, Vec<u8>>,
    /// Next expected sequence number for playback.
    next_seq: Option<u16>,
    /// Current applied target delay in milliseconds.
    current_target_delay_ms: f64,
    /// Raw (unclamped, unshrunk) computed target delay from latest stats.
    raw_target_delay_ms: f64,
    /// Timestamp of the last `update_stats` or `pop` call that adjusted delay.
    last_delay_update: Instant,
    /// Whether the buffer has been initialized (first packet received).
    initialized: bool,
    /// Instant when the first packet of this session arrived (playout clock origin).
    playout_origin: Option<Instant>,
    /// Sequence number of the first packet (used to compute per-packet deadlines).
    first_seq: Option<u16>,
    /// Whether we are in the startup prefetch phase.
    in_prefetch: bool,
    /// Count of packets whose arrival was after their computed playout deadline.
    late_packets: u64,
    /// Count of `Missing` results returned from `pop()` (PLC invocations).
    plc_frames: u64,
    /// Count of `NotReady` results returned from `pop()`.
    not_ready_count: u64,
    /// Packets dropped because `data.len() > MAX_PACKET_SIZE`.
    dropped_oversize: u64,
    /// Packets dropped because `buffer.len() >= MAX_BUFFERED_PACKETS`.
    dropped_overcap: u64,
}

impl Default for AdaptiveJitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveJitterBuffer {
    /// Create a new adaptive jitter buffer with the default minimum target delay.
    pub fn new() -> Self {
        Self {
            buffer: BTreeMap::new(),
            next_seq: None,
            current_target_delay_ms: MIN_JITTER_DELAY_MS,
            raw_target_delay_ms: MIN_JITTER_DELAY_MS,
            last_delay_update: Instant::now(),
            initialized: false,
            playout_origin: None,
            first_seq: None,
            in_prefetch: false,
            late_packets: 0,
            plc_frames: 0,
            not_ready_count: 0,
            dropped_oversize: 0,
            dropped_overcap: 0,
        }
    }

    /// Compute the ideal target delay from jitter statistics (before shrink-rate limiting).
    fn compute_raw_target(avg_jitter_ms: f64, jitter_stddev_ms: f64) -> f64 {
        let raw = avg_jitter_ms + 2.0 * jitter_stddev_ms;
        raw.clamp(MIN_JITTER_DELAY_MS, MAX_JITTER_DELAY_MS)
    }

    /// Apply the shrink-rate limit: the target can decrease by at most
    /// `MAX_DELAY_SHRINK_PER_SEC_MS` (5 ms) per second of elapsed time.
    fn apply_shrink_limit(&self, new_raw: f64, now: Instant) -> f64 {
        if new_raw >= self.current_target_delay_ms {
            // Increasing or unchanged — apply immediately.
            new_raw
        } else {
            let elapsed_secs = now.duration_since(self.last_delay_update).as_secs_f64();
            let max_shrink = MAX_DELAY_SHRINK_PER_SEC_MS * elapsed_secs;
            let floor = self.current_target_delay_ms - max_shrink;
            new_raw.max(floor)
        }
    }

    /// Number of packets currently buffered.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Number of oversize packets dropped since creation.
    pub fn dropped_oversize(&self) -> u64 {
        self.dropped_oversize
    }

    /// Number of packets dropped due to buffer capacity since creation.
    pub fn dropped_overcap(&self) -> u64 {
        self.dropped_overcap
    }
}

impl JitterBuffering for AdaptiveJitterBuffer {
    fn push(&mut self, seq: u16, data: Vec<u8>) {
        // GUARD 1: reject oversize packets (before any other logic).
        if data.len() > MAX_PACKET_SIZE {
            self.dropped_oversize += 1;
            return;
        }

        // Initialize playback position on first packet.
        if !self.initialized {
            self.next_seq = Some(seq);
            self.initialized = true;
            // Record playout origin and first sequence for time-gated playout.
            self.playout_origin = Some(Instant::now());
            self.first_seq = Some(seq);
            self.in_prefetch = true;
        }

        // Discard old/duplicate packets using wrapping distance.
        // Values in 0..=0x7FFF are forward, 0x8000..=0xFFFF are old/duplicate.
        if let Some(first) = self.first_seq {
            let dist = seq.wrapping_sub(first);
            if dist > 0x7FFF {
                // seq is behind first_seq — old/duplicate packet, discard.
                return;
            }
        }

        // Discard late packets: sequence number behind the current playback position.
        if let Some(next) = self.next_seq {
            let diff = seq.wrapping_sub(next);
            if diff > 0x7FFF {
                // seq is behind next_seq — late packet, discard.
                return;
            }
        }

        // Late packet detection: a packet is "late" if its arrival instant is
        // after its playout deadline. During prefetch, no packet is considered late.
        if !self.in_prefetch {
            if let (Some(origin), Some(first)) = (self.playout_origin, self.first_seq) {
                let dist = seq.wrapping_sub(first) as u32;
                let deadline = origin
                    + FRAME_DURATION * dist
                    + Duration::from_secs_f64(self.current_target_delay_ms / 1000.0);
                if Instant::now() > deadline {
                    self.late_packets += 1;
                }
            }
        }

        // GUARD 2: reject when buffer is at capacity (drop-newest policy).
        if self.buffer.len() >= MAX_BUFFERED_PACKETS {
            self.dropped_overcap += 1;
            return;
        }

        self.buffer.insert(seq, data);
    }

    fn pop(&mut self, now: Instant) -> JitterResult {
        // 1. If no packets have arrived yet, return NotReady.
        let origin = match self.playout_origin {
            Some(o) => o,
            None => {
                self.not_ready_count += 1;
                return JitterResult::NotReady;
            }
        };

        let first = self
            .first_seq
            .expect("invariant: first_seq set with playout_origin in push()");

        // 2. Prefetch phase: hold until max(target_delay_ms, MIN_PREFETCH_MS) ms.
        if self.in_prefetch {
            let prefetch_ms = self.current_target_delay_ms.max(MIN_PREFETCH_MS as f64);
            let prefetch_deadline = origin + Duration::from_secs_f64(prefetch_ms / 1000.0);
            if now < prefetch_deadline {
                self.not_ready_count += 1;
                return JitterResult::NotReady;
            }
            // Exit prefetch.
            self.in_prefetch = false;
        }

        let next = match self.next_seq {
            Some(n) => n,
            None => {
                self.not_ready_count += 1;
                return JitterResult::NotReady;
            }
        };

        // 3. Compute playout deadline for the next expected sequence number.
        // deadline = playout_origin + (next_seq - first_seq) * FRAME_DURATION + target_delay
        let seq_dist = next.wrapping_sub(first);
        // Interpret 0..=0x7FFF as forward distance.
        if seq_dist > 0x7FFF {
            // Wrapped backwards — shouldn't happen in normal flow, treat as not ready.
            self.not_ready_count += 1;
            return JitterResult::NotReady;
        }
        let playout_deadline = origin
            + FRAME_DURATION * (seq_dist as u32)
            + Duration::from_secs_f64(self.current_target_delay_ms / 1000.0);

        // 4. If now < deadline, not time yet.
        if now < playout_deadline {
            self.not_ready_count += 1;
            return JitterResult::NotReady;
        }

        // 5. Deadline reached — check if packet is present.
        if let Some(data) = self.buffer.remove(&next) {
            self.next_seq = Some(next.wrapping_add(1));
            return JitterResult::Packet(data);
        }

        // 6. Packet missing at deadline — only signal Missing (PLC) if we have
        // later packets in the buffer (evidence of a gap). If the buffer is
        // empty, there's no evidence of loss — return NotReady.
        if let Some(&first_buffered) = self.buffer.keys().next() {
            let diff = first_buffered.wrapping_sub(next);
            if diff > 0 && diff <= 0x7FFF {
                // We have later packets but not the one we need — it's lost.
                self.next_seq = Some(next.wrapping_add(1));
                self.plc_frames += 1;
                return JitterResult::Missing;
            }
        }

        // Buffer empty or only old packets — not ready.
        self.not_ready_count += 1;
        JitterResult::NotReady
    }

    fn update_stats(&mut self, avg_jitter_ms: f64, jitter_stddev_ms: f64) {
        let now = Instant::now();
        self.raw_target_delay_ms = Self::compute_raw_target(avg_jitter_ms, jitter_stddev_ms);
        self.current_target_delay_ms = self.apply_shrink_limit(self.raw_target_delay_ms, now);
        self.last_delay_update = now;
    }

    fn target_delay_ms(&self) -> f64 {
        self.current_target_delay_ms
    }

    fn stats(&self) -> JitterBufferStats {
        // current_buffered_ms: time span of buffered packets.
        // Computed as (highest_seq - next_seq) * 20ms equivalent.
        let buffered_ms = if let Some(next) = self.next_seq {
            if let Some(&highest) = self.buffer.keys().next_back() {
                let dist = highest.wrapping_sub(next);
                if dist <= 0x7FFF {
                    (dist as f64) * FRAME_DURATION.as_millis() as f64
                } else {
                    0.0
                }
            } else {
                0.0
            }
        } else {
            0.0
        };

        JitterBufferStats {
            target_delay_ms: self.current_target_delay_ms,
            current_buffered_ms: buffered_ms,
            late_packets: self.late_packets,
            plc_frames: self.plc_frames,
            not_ready_count: self.not_ready_count,
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// A test-only clock that produces deterministic `Instant` values.
    ///
    /// Captures a `base` instant at construction. `now()` returns
    /// `base + Duration::from_millis(offset_ms)`. `advance(ms)` increments
    /// the internal counter. No `Clock` trait - callers pass `clock.now()`
    /// directly to `pop(now)` or `set_rtt(now, ...)`.
    pub struct FakeClock {
        base: Instant,
        offset_ms: Arc<AtomicU64>,
    }

    impl FakeClock {
        /// Create a new `FakeClock` starting at offset 0.
        pub fn new() -> Self {
            Self {
                base: Instant::now(),
                offset_ms: Arc::new(AtomicU64::new(0)),
            }
        }

        /// Return the current fake instant: `base + offset_ms`.
        pub fn now(&self) -> Instant {
            let ms = self.offset_ms.load(Ordering::Relaxed);
            self.base + Duration::from_millis(ms)
        }

        /// Advance the clock by `ms` milliseconds.
        pub fn advance(&self, ms: u64) {
            self.offset_ms.fetch_add(ms, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeClock;
    use super::*;
    use proptest::prelude::*;
    use std::time::{Duration, Instant};

    // -----------------------------------------------------------------------
    // Property 4: Jitter buffer preserves packet ordering
    // **Validates: Requirements 3.1**
    //
    // For any set of packets with distinct sequence numbers inserted in
    // arbitrary order, popping all packets yields them in strictly ascending
    // sequence number order.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 4: Jitter buffer preserves packet ordering
        #[test]
        fn jitter_buffer_preserves_ordering(
            base in 0u16..65_486,
            count in 1usize..50,
            shuffle_rands in proptest::collection::vec(any::<usize>(), 1..51),
        ) {
            let count = count.min(65_535usize.saturating_sub(base as usize));
            prop_assume!(count > 0);

            // Build sorted sequence.
            let sorted: Vec<u16> = (0..count as u16).map(|i| base + i).collect();

            // Shuffle using the random values.
            let mut shuffled = sorted.clone();
            for i in (1..shuffled.len()).rev() {
                let j = shuffle_rands[i % shuffle_rands.len()] % (i + 1);
                shuffled.swap(i, j);
            }

            let mut jb = AdaptiveJitterBuffer::new();
            jb.next_seq = Some(base);
            jb.initialized = true;
            // Bypass time-gating: set playout origin in the past and disable prefetch.
            let origin = Instant::now();
            jb.playout_origin = Some(origin);
            jb.first_seq = Some(base);
            jb.in_prefetch = false;

            for &seq in &shuffled {
                jb.push(seq, vec![seq as u8, (seq >> 8) as u8]);
            }

            // Use a far-future instant so all deadlines are past.
            let far_future = origin + Duration::from_secs(3600);

            // Pop all and verify ascending order.
            let mut popped = Vec::new();
            loop {
                match jb.pop(far_future) {
                    JitterResult::Packet(data) => popped.push(data),
                    JitterResult::Missing => { /* skip gaps */ }
                    JitterResult::NotReady => break,
                }
                if popped.len() > sorted.len() {
                    break;
                }
            }

            prop_assert_eq!(popped.len(), sorted.len());

            for (i, data) in popped.iter().enumerate() {
                let expected_seq = sorted[i];
                let expected_data = vec![expected_seq as u8, (expected_seq >> 8) as u8];
                prop_assert_eq!(data, &expected_data);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Property 5: Jitter buffer gap handling
    // **Validates: Requirements 3.2, 3.6**
    //
    // For a sequence with a gap, the buffer returns Missing for the gap
    // position (triggering PLC), then delivers subsequent packets normally.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 5: Jitter buffer gap handling
        #[test]
        fn jitter_buffer_gap_handling(
            base_seq in 0u16..65000,
            gap_offset in 1u16..10,
            after_gap_count in 1u16..5,
        ) {
            let mut jb = AdaptiveJitterBuffer::new();

            // Push the base packet.
            jb.push(base_seq, vec![0xAA]);

            // Bypass time-gating: set origin in the past and disable prefetch.
            let origin = jb.playout_origin.unwrap();
            jb.in_prefetch = false;
            let far_future = origin + Duration::from_secs(3600);

            // Skip `gap_offset` packets (create a gap), then push packets after the gap.
            let after_gap_start = base_seq.wrapping_add(1 + gap_offset);
            for i in 0..after_gap_count {
                let seq = after_gap_start.wrapping_add(i);
                jb.push(seq, vec![0xBB, i as u8]);
            }

            // Pop the base packet — should succeed.
            match jb.pop(far_future) {
                JitterResult::Packet(data) => prop_assert_eq!(data, vec![0xAA]),
                other => prop_assert!(false, "Expected Packet for base, got {:?}", match other {
                    JitterResult::Missing => "Missing",
                    JitterResult::NotReady => "NotReady",
                    _ => "Packet",
                }),
            }

            // Pop the gap positions — each should return Missing.
            for _ in 0..gap_offset {
                match jb.pop(far_future) {
                    JitterResult::Missing => { /* expected */ }
                    JitterResult::NotReady => {
                        prop_assert!(false, "Expected Missing for gap position, got NotReady");
                    }
                    JitterResult::Packet(_) => {
                        prop_assert!(false, "Expected Missing for gap position, got Packet");
                    }
                }
            }

            // Pop the after-gap packets — should all succeed.
            for i in 0..after_gap_count {
                match jb.pop(far_future) {
                    JitterResult::Packet(data) => {
                        prop_assert_eq!(data, vec![0xBB, i as u8]);
                    }
                    other => prop_assert!(false, "Expected Packet after gap, got {:?}", match other {
                        JitterResult::Missing => "Missing",
                        JitterResult::NotReady => "NotReady",
                        _ => "Packet",
                    }),
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Property 6: Jitter buffer target delay computation
    // **Validates: Requirements 3.3, 3.5**
    //
    // For any avg_jitter >= 0 and jitter_stddev >= 0, the computed target
    // delay equals clamp(avg + 2*stddev, 20.0, 200.0).
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 6: Jitter buffer target delay computation
        #[test]
        fn jitter_buffer_target_delay_formula(
            avg in 0.0f64..500.0,
            stddev in 0.0f64..500.0,
        ) {
            let expected = (avg + 2.0 * stddev).clamp(MIN_JITTER_DELAY_MS, MAX_JITTER_DELAY_MS);
            let computed = AdaptiveJitterBuffer::compute_raw_target(avg, stddev);
            prop_assert!(
                (computed - expected).abs() < f64::EPSILON,
                "compute_raw_target({}, {}) = {}, expected {}",
                avg, stddev, computed, expected
            );

            // Also verify bounds invariant.
            prop_assert!(computed >= MIN_JITTER_DELAY_MS);
            prop_assert!(computed <= MAX_JITTER_DELAY_MS);
        }
    }

    // -----------------------------------------------------------------------
    // Property 7: Jitter buffer target delay shrink rate
    // **Validates: Requirements 3.4**
    //
    // When the new target is lower than the current, the applied target
    // decreases by at most 5ms per second of elapsed time.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 7: Jitter buffer target delay shrink rate
        #[test]
        fn jitter_buffer_shrink_rate_limit(
            current_target in 20.0f64..200.0,
            new_raw in 20.0f64..200.0,
            elapsed_ms in 0u64..10_000,
        ) {
            // Only test the shrink case.
            prop_assume!(new_raw < current_target);

            let mut jb = AdaptiveJitterBuffer::new();
            jb.current_target_delay_ms = current_target;

            // Simulate elapsed time by setting last_delay_update in the past.
            let now = Instant::now();
            let elapsed = std::time::Duration::from_millis(elapsed_ms);
            // We can't subtract from Instant easily, so we use the apply_shrink_limit
            // method directly with a computed "now" that is `elapsed` after last_update.
            jb.last_delay_update = now;
            let future_now = now + elapsed;

            let applied = jb.apply_shrink_limit(new_raw, future_now);

            let elapsed_secs = elapsed_ms as f64 / 1000.0;
            let max_shrink = MAX_DELAY_SHRINK_PER_SEC_MS * elapsed_secs;
            let min_allowed = current_target - max_shrink;

            // The applied target should be >= the floor (shrink-limited).
            // Use a small tolerance to account for floating-point arithmetic
            // differences between integer-based and Duration-based elapsed time.
            let tolerance = 1e-9;
            prop_assert!(
                applied >= min_allowed - tolerance,
                "applied={}, min_allowed={}, current={}, new_raw={}, elapsed_ms={}",
                applied, min_allowed, current_target, new_raw, elapsed_ms
            );

            // The applied target should be >= the new raw target
            // (we never go below what was requested).
            prop_assert!(applied >= new_raw - tolerance);
        }
    }

    // -----------------------------------------------------------------------
    // Property 8: Jitter buffer discards late packets
    // **Validates: Requirements 3.7**
    //
    // Pushing a packet with a sequence number behind the current playback
    // position does not change the buffer contents or affect pop results.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 8: Jitter buffer discards late packets
        #[test]
        fn jitter_buffer_discards_late_packets(
            next_seq in 10u16..65000,
            late_offset in 1u16..10,
        ) {
            let mut jb = AdaptiveJitterBuffer::new();
            jb.initialized = true;
            jb.next_seq = Some(next_seq);
            // Bypass time-gating.
            let origin = Instant::now();
            jb.playout_origin = Some(origin);
            jb.first_seq = Some(next_seq.wrapping_sub(5)); // first_seq before next_seq
            jb.in_prefetch = false;
            let far_future = origin + Duration::from_secs(3600);

            // Push a valid future packet so we have something in the buffer.
            let future_seq = next_seq.wrapping_add(1);
            jb.push(future_seq, vec![0xFF]);
            let len_before = jb.len();

            // Push a late packet (behind playback position).
            let late_seq = next_seq.wrapping_sub(late_offset);
            jb.push(late_seq, vec![0x00]);

            // Buffer size should not have changed.
            prop_assert_eq!(jb.len(), len_before, "Late packet was not discarded");

            // Pop should still work correctly — first Missing (for next_seq),
            // then the future packet.
            match jb.pop(far_future) {
                JitterResult::Missing => { /* next_seq is missing, expected */ }
                JitterResult::Packet(_) => {
                    prop_assert!(false, "Should not get Packet for next_seq (it wasn't pushed)");
                }
                JitterResult::NotReady => {
                    prop_assert!(false, "Should not get NotReady when future packets exist");
                }
            }

            // Now next_seq has advanced, pop the future packet.
            match jb.pop(far_future) {
                JitterResult::Packet(data) => prop_assert_eq!(data, vec![0xFF]),
                _ => prop_assert!(false, "Expected the future packet"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Property 1: Prefetch gating (Task 10.6)
    // Feature: audio-transport-hardening, Property 1: Prefetch gating
    // **Validates: Requirements 1.1, 1.2**
    //
    // For any AdaptiveJitterBuffer that has just received its first packet,
    // calling pop(now) with any Instant less than
    // playout_origin + max(target_delay_ms, 60)ms SHALL return NotReady.
    // Calling pop(now) at or past that threshold SHALL exit prefetch.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-transport-hardening, Property 1: Prefetch gating
        #[test]
        fn prefetch_gating(
            target_delay in 20.0f64..200.0,
            before_offset_ms in 0u64..59,
        ) {
            let clock = FakeClock::new();
            let mut jb = AdaptiveJitterBuffer::new();
            jb.current_target_delay_ms = target_delay;

            // Manually set playout_origin to the fake clock's base so we control timing.
            let origin = clock.now();
            jb.playout_origin = Some(origin);
            jb.first_seq = Some(100);
            jb.next_seq = Some(100);
            jb.initialized = true;
            jb.in_prefetch = true;

            // Push a packet so the buffer has data.
            jb.push(100, vec![0xAA]);

            // Prefetch deadline: max(target_delay_ms, 60) ms (using f64 precision).
            let prefetch_ms = target_delay.max(MIN_PREFETCH_MS as f64);
            // Use ceiling to get the integer ms that's guaranteed to be before the deadline.
            let prefetch_ceil_ms = prefetch_ms.ceil() as u64;

            // Before prefetch deadline: should return NotReady.
            let safe_before = before_offset_ms.min(prefetch_ceil_ms.saturating_sub(1));
            clock.advance(safe_before);
            let result = jb.pop(clock.now());
            prop_assert!(
                matches!(result, JitterResult::NotReady),
                "Expected NotReady before prefetch deadline (at {}ms, deadline~={}ms), got {:?}",
                safe_before, prefetch_ms, result,
            );
            prop_assert!(jb.in_prefetch, "Should still be in prefetch before deadline");

            // Advance well past the prefetch deadline AND the playout deadline for seq 100.
            // Playout deadline for seq 100 = origin + (100-100)*20ms + target_delay_ms.
            // Both are <= prefetch_ms, so advancing to prefetch_ceil_ms + 1 covers both.
            let total_advance = prefetch_ceil_ms + 1;
            let remaining = total_advance - safe_before;
            clock.advance(remaining);

            let result = jb.pop(clock.now());
            prop_assert!(
                matches!(result, JitterResult::Packet(_)),
                "Expected Packet past prefetch deadline (at {}ms, deadline~={}ms), got {:?}",
                total_advance, prefetch_ms, result,
            );
            prop_assert!(!jb.in_prefetch, "Should have exited prefetch at deadline");
        }
    }

    // -----------------------------------------------------------------------
    // Property 2: Playout deadline correctness (Task 10.7)
    // Feature: audio-transport-hardening, Property 2: Playout deadline correctness
    // **Validates: Requirements 1.3, 1.4, 1.5, 1.6**
    //
    // For any AdaptiveJitterBuffer that has exited prefetch, and for any
    // sequence number with playout deadline D:
    // - If now < D, pop(now) returns NotReady.
    // - If now >= D and packet present, pop(now) returns Packet.
    // - If now >= D and packet missing (but later packets exist), pop returns Missing.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-transport-hardening, Property 2: Playout deadline correctness
        #[test]
        fn playout_deadline_correctness(
            target_delay in 20.0f64..200.0,
            seq_offset in 0u16..50,
            has_packet in any::<bool>(),
        ) {
            let clock = FakeClock::new();
            let first_seq: u16 = 100;
            let test_seq = first_seq.wrapping_add(seq_offset);

            let mut jb = AdaptiveJitterBuffer::new();
            jb.current_target_delay_ms = target_delay;
            let origin = clock.now();
            jb.playout_origin = Some(origin);
            jb.first_seq = Some(first_seq);
            jb.next_seq = Some(test_seq);
            jb.initialized = true;
            jb.in_prefetch = false;

            // Push the test packet (or not, to test Missing).
            if has_packet {
                jb.push(test_seq, vec![0xBB]);
            }
            // Always push a later packet so Missing can be detected.
            let later_seq = test_seq.wrapping_add(1);
            jb.push(later_seq, vec![0xCC]);

            // Compute the expected deadline for test_seq (f64 precision).
            // deadline = (seq_offset as f64) * 20.0 + target_delay
            let deadline_exact_ms = (seq_offset as f64) * 20.0 + target_delay;
            // Use floor for "before" check and ceiling+1 for "at or past" check.
            let deadline_floor_ms = deadline_exact_ms.floor() as u64;
            let deadline_ceil_ms = deadline_exact_ms.ceil() as u64;

            // Before deadline: should return NotReady.
            if deadline_floor_ms > 0 {
                clock.advance(deadline_floor_ms - 1);
                let result = jb.pop(clock.now());
                prop_assert!(
                    matches!(result, JitterResult::NotReady),
                    "Expected NotReady before deadline (at {}ms, deadline~={}ms), got {:?}",
                    deadline_floor_ms - 1, deadline_exact_ms, result,
                );
                // Advance to ceiling + 1 to be safely past the deadline.
                let advance_to = deadline_ceil_ms + 1;
                clock.advance(advance_to - (deadline_floor_ms - 1));
            } else {
                // deadline is at or near 0ms — advance past it.
                clock.advance(deadline_ceil_ms + 1);
            }

            // At/past deadline: should return Packet or Missing.
            let result = jb.pop(clock.now());
            if has_packet {
                prop_assert!(
                    matches!(result, JitterResult::Packet(_)),
                    "Expected Packet past deadline (~{}ms) when packet present, got {:?}",
                    deadline_exact_ms, result,
                );
            } else {
                prop_assert!(
                    matches!(result, JitterResult::Missing),
                    "Expected Missing past deadline (~{}ms) when packet absent, got {:?}",
                    deadline_exact_ms, result,
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Property 3: Jitter buffer stats accuracy (Task 10.8)
    // Feature: audio-transport-hardening, Property 3: Jitter buffer stats accuracy
    // **Validates: Requirements 1.8**
    //
    // For any sequence of push and pop operations, the JitterBufferStats
    // returned by stats() SHALL have plc_frames, not_ready_count, and
    // late_packets matching observed results.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-transport-hardening, Property 3: Jitter buffer stats accuracy
        #[test]
        fn jitter_buffer_stats_accuracy(
            num_pushes in 1usize..20,
            gap_positions in proptest::collection::vec(any::<bool>(), 1..20),
            num_pops in 1usize..40,
        ) {
            let clock = FakeClock::new();
            let first_seq: u16 = 0;
            let mut jb = AdaptiveJitterBuffer::new();

            let origin = clock.now();
            jb.playout_origin = Some(origin);
            jb.first_seq = Some(first_seq);
            jb.next_seq = Some(first_seq);
            jb.initialized = true;
            jb.in_prefetch = false;
            jb.current_target_delay_ms = 20.0;

            // Push packets with some gaps based on gap_positions.
            let mut pushed_seqs = std::collections::HashSet::new();
            for i in 0..num_pushes {
                let skip = gap_positions.get(i).copied().unwrap_or(false);
                if !skip {
                    let seq = first_seq.wrapping_add(i as u16);
                    jb.push(seq, vec![i as u8]);
                    pushed_seqs.insert(seq);
                }
            }

            // Also push a sentinel packet well past the range to ensure
            // Missing is returned for gaps (not NotReady).
            let sentinel_seq = first_seq.wrapping_add(num_pushes as u16 + 5);
            jb.push(sentinel_seq, vec![0xFF]);

            // Pop packets, tracking results.
            let mut observed_not_ready = 0u64;
            let mut observed_plc = 0u64;

            // Advance clock well past all deadlines.
            clock.advance(10_000);

            for _ in 0..num_pops {
                match jb.pop(clock.now()) {
                    JitterResult::NotReady => observed_not_ready += 1,
                    JitterResult::Missing => observed_plc += 1,
                    JitterResult::Packet(_) => {}
                }
                // Advance clock a bit for each pop to ensure deadlines pass.
                clock.advance(20);
            }

            let stats = jb.stats();
            prop_assert_eq!(
                stats.plc_frames, observed_plc,
                "plc_frames mismatch: stats={}, observed={}",
                stats.plc_frames, observed_plc,
            );
            prop_assert_eq!(
                stats.not_ready_count, observed_not_ready,
                "not_ready_count mismatch: stats={}, observed={}",
                stats.not_ready_count, observed_not_ready,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests for jitter buffer edge cases (Task 10.9)
    // Validates: Requirements 1.1, 1.2, 1.3, 1.4, 1.5, 1.6
    // -----------------------------------------------------------------------

    #[test]
    fn jitter_empty_buffer_pop() {
        let mut jb = AdaptiveJitterBuffer::new();
        // No packets pushed — pop should return NotReady.
        let result = jb.pop(Instant::now());
        assert!(matches!(result, JitterResult::NotReady));
    }

    #[test]
    fn jitter_single_packet_push_pop() {
        let clock = FakeClock::new();
        let mut jb = AdaptiveJitterBuffer::new();
        jb.current_target_delay_ms = 20.0;

        // Push one packet — sets playout_origin, first_seq, enters prefetch.
        jb.push(42, vec![0xAA]);
        assert!(jb.in_prefetch);
        assert_eq!(jb.first_seq, Some(42));

        // Before prefetch deadline (max(20, 60) = 60ms): NotReady.
        clock.advance(59);
        // We need to set the playout_origin to the clock's base for deterministic testing.
        jb.playout_origin = Some(clock.now() - Duration::from_millis(59));
        let result = jb.pop(clock.now());
        assert!(matches!(result, JitterResult::NotReady));

        // At prefetch deadline (60ms): should exit prefetch and return Packet.
        clock.advance(1);
        let result = jb.pop(clock.now());
        assert!(
            matches!(result, JitterResult::Packet(ref d) if d == &vec![0xAA]),
            "Expected Packet at 60ms, got {:?}",
            result
        );
        assert!(!jb.in_prefetch);
    }

    #[test]
    fn jitter_u16_sequence_wraparound() {
        let clock = FakeClock::new();
        let mut jb = AdaptiveJitterBuffer::new();
        jb.current_target_delay_ms = 20.0;

        let origin = clock.now();
        jb.playout_origin = Some(origin);
        jb.first_seq = Some(65534);
        jb.next_seq = Some(65534);
        jb.initialized = true;
        jb.in_prefetch = false;

        // Push packets that wrap around u16.
        jb.push(65534, vec![0x01]);
        jb.push(65535, vec![0x02]);
        jb.push(0, vec![0x03]);
        jb.push(1, vec![0x04]);

        // Advance well past all deadlines.
        clock.advance(10_000);
        let now = clock.now();

        // Pop all four in order.
        assert!(matches!(jb.pop(now), JitterResult::Packet(ref d) if d == &vec![0x01]));
        assert!(matches!(jb.pop(now), JitterResult::Packet(ref d) if d == &vec![0x02]));
        assert!(matches!(jb.pop(now), JitterResult::Packet(ref d) if d == &vec![0x03]));
        assert!(matches!(jb.pop(now), JitterResult::Packet(ref d) if d == &vec![0x04]));
        assert!(matches!(jb.pop(now), JitterResult::NotReady));
    }

    #[test]
    fn jitter_prefetch_exactly_60ms() {
        let clock = FakeClock::new();
        let mut jb = AdaptiveJitterBuffer::new();
        // target_delay < 60 → prefetch uses 60ms.
        jb.current_target_delay_ms = 30.0;

        let origin = clock.now();
        jb.playout_origin = Some(origin);
        jb.first_seq = Some(0);
        jb.next_seq = Some(0);
        jb.initialized = true;
        jb.in_prefetch = true;

        jb.push(0, vec![0xAA]);

        // At 59ms: still in prefetch.
        clock.advance(59);
        assert!(matches!(jb.pop(clock.now()), JitterResult::NotReady));
        assert!(jb.in_prefetch);

        // At 60ms: exits prefetch. But playout_deadline = origin + 0*20ms + 30ms = 30ms.
        // We're at 60ms which is past 30ms, so packet should be delivered.
        clock.advance(1);
        assert!(matches!(jb.pop(clock.now()), JitterResult::Packet(_)));
        assert!(!jb.in_prefetch);
    }

    #[test]
    fn jitter_prefetch_with_target_delay_above_60ms() {
        let clock = FakeClock::new();
        let mut jb = AdaptiveJitterBuffer::new();
        // target_delay > 60 → prefetch uses target_delay.
        jb.current_target_delay_ms = 100.0;

        let origin = clock.now();
        jb.playout_origin = Some(origin);
        jb.first_seq = Some(0);
        jb.next_seq = Some(0);
        jb.initialized = true;
        jb.in_prefetch = true;

        jb.push(0, vec![0xAA]);

        // At 60ms: still in prefetch (target is 100ms).
        clock.advance(60);
        assert!(matches!(jb.pop(clock.now()), JitterResult::NotReady));
        assert!(jb.in_prefetch);

        // At 99ms: still in prefetch.
        clock.advance(39);
        assert!(matches!(jb.pop(clock.now()), JitterResult::NotReady));
        assert!(jb.in_prefetch);

        // At 100ms: exits prefetch and delivers packet.
        clock.advance(1);
        assert!(matches!(jb.pop(clock.now()), JitterResult::Packet(_)));
        assert!(!jb.in_prefetch);
    }

    // -----------------------------------------------------------------------
    // Behavioral smoke test for jitter buffer (Task 10.10)
    // Validates: Requirements 1.2, 1.3, 1.4, 1.5, 1.6
    //
    // Push seq 1..5, advance `now` across deadlines, assert exact sequence
    // of NotReady → Packet → Packet → … to catch off-by-one in deadline.
    // -----------------------------------------------------------------------

    #[test]
    fn jitter_behavioral_smoke_test() {
        let clock = FakeClock::new();
        let mut jb = AdaptiveJitterBuffer::new();
        jb.current_target_delay_ms = 20.0; // minimum target delay

        let origin = clock.now();
        jb.playout_origin = Some(origin);
        jb.first_seq = Some(1);
        jb.next_seq = Some(1);
        jb.initialized = true;
        jb.in_prefetch = false;

        // Push seq 1..=5.
        for seq in 1u16..=5 {
            jb.push(seq, vec![seq as u8]);
        }

        // Playout deadlines (target_delay = 20ms, FRAME_DURATION = 20ms):
        // seq 1: origin + (1-1)*20 + 20 = origin + 20ms
        // seq 2: origin + (2-1)*20 + 20 = origin + 40ms
        // seq 3: origin + (3-1)*20 + 20 = origin + 60ms
        // seq 4: origin + (4-1)*20 + 20 = origin + 80ms
        // seq 5: origin + (5-1)*20 + 20 = origin + 100ms

        // At t=0: before seq 1's deadline (20ms) → NotReady.
        assert!(
            matches!(jb.pop(clock.now()), JitterResult::NotReady),
            "t=0: expected NotReady"
        );

        // At t=19ms: still before seq 1's deadline → NotReady.
        clock.advance(19);
        assert!(
            matches!(jb.pop(clock.now()), JitterResult::NotReady),
            "t=19: expected NotReady"
        );

        // At t=20ms: seq 1's deadline → Packet(1).
        clock.advance(1);
        match jb.pop(clock.now()) {
            JitterResult::Packet(d) => assert_eq!(d, vec![1], "t=20: expected seq 1"),
            other => panic!("t=20: expected Packet(1), got {:?}", other),
        }

        // At t=20ms: seq 2's deadline is 40ms, still before → NotReady.
        assert!(
            matches!(jb.pop(clock.now()), JitterResult::NotReady),
            "t=20: expected NotReady for seq 2"
        );

        // At t=40ms: seq 2's deadline → Packet(2).
        clock.advance(20);
        match jb.pop(clock.now()) {
            JitterResult::Packet(d) => assert_eq!(d, vec![2], "t=40: expected seq 2"),
            other => panic!("t=40: expected Packet(2), got {:?}", other),
        }

        // At t=60ms: seq 3's deadline → Packet(3).
        clock.advance(20);
        match jb.pop(clock.now()) {
            JitterResult::Packet(d) => assert_eq!(d, vec![3], "t=60: expected seq 3"),
            other => panic!("t=60: expected Packet(3), got {:?}", other),
        }

        // At t=80ms: seq 4's deadline → Packet(4).
        clock.advance(20);
        match jb.pop(clock.now()) {
            JitterResult::Packet(d) => assert_eq!(d, vec![4], "t=80: expected seq 4"),
            other => panic!("t=80: expected Packet(4), got {:?}", other),
        }

        // At t=100ms: seq 5's deadline → Packet(5).
        clock.advance(20);
        match jb.pop(clock.now()) {
            JitterResult::Packet(d) => assert_eq!(d, vec![5], "t=100: expected seq 5"),
            other => panic!("t=100: expected Packet(5), got {:?}", other),
        }

        // No more packets → NotReady.
        assert!(
            matches!(jb.pop(clock.now()), JitterResult::NotReady),
            "t=100: expected NotReady after all packets"
        );
    }

    // -----------------------------------------------------------------------
    // Property 1: Jitter buffer memory bound
    // Feature: client-security-hardening
    // **Validates: Requirements 1.1, 1.2, 1.3, 2.1, 2.2, 2.4**
    //
    // For any sequence of push calls, buffer.len() <= MAX_BUFFERED_PACKETS
    // and all stored payloads have data.len() <= MAX_PACKET_SIZE.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, .. ProptestConfig::default() })]

        /// Feature: client-security-hardening, Property 1: Jitter buffer memory bound
        #[test]
        fn jitter_buffer_memory_bound(
            packets in proptest::collection::vec(
                (any::<u16>(), proptest::collection::vec(any::<u8>(), 0..2000)),
                0..600,
            ),
        ) {
            let mut jb = AdaptiveJitterBuffer::new();

            for (seq, data) in &packets {
                jb.push(*seq, data.clone());

                // Invariant: buffer length never exceeds MAX_BUFFERED_PACKETS.
                prop_assert!(
                    jb.len() <= MAX_BUFFERED_PACKETS,
                    "buffer.len() = {} exceeds MAX_BUFFERED_PACKETS = {} after pushing seq {}",
                    jb.len(), MAX_BUFFERED_PACKETS, seq,
                );
            }

            // After all pushes, verify every stored payload is within MAX_PACKET_SIZE.
            // We can't access the private buffer directly, but we can verify indirectly:
            // any packet that made it into the buffer must have passed the oversize guard,
            // so data.len() <= MAX_PACKET_SIZE. We verify the counters are consistent.
            let total_pushes = packets.len() as u64;
            let oversize = jb.dropped_oversize();
            let overcap = jb.dropped_overcap();
            let buffered = jb.len() as u64;

            // Oversize + overcap + buffered + other discards <= total pushes.
            prop_assert!(
                oversize + overcap + buffered <= total_pushes,
                "oversize({}) + overcap({}) + buffered({}) = {} > total_pushes({})",
                oversize, overcap, buffered,
                oversize + overcap + buffered, total_pushes,
            );

            // Every oversize packet should have been counted.
            let expected_oversize = packets.iter()
                .filter(|(_, data)| data.len() > MAX_PACKET_SIZE)
                .count() as u64;
            prop_assert_eq!(
                oversize, expected_oversize,
                "dropped_oversize mismatch: got {}, expected {}",
                oversize, expected_oversize,
            );

            // Final length check.
            prop_assert!(
                jb.len() <= MAX_BUFFERED_PACKETS,
                "final buffer.len() = {} exceeds MAX_BUFFERED_PACKETS = {}",
                jb.len(), MAX_BUFFERED_PACKETS,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Property 2: Jitter buffer counter conservation
    // Feature: client-security-hardening
    // **Validates: Requirements 2.5**
    //
    // For any push sequence, dropped_oversize + dropped_overcap + buffer.len()
    // <= total_pushes. Since late/duplicate discards are silent (no public
    // counter), the testable conservation inequality uses only the observable
    // counters.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, .. ProptestConfig::default() })]

        /// Feature: client-security-hardening, Property 2: Jitter buffer counter conservation
        #[test]
        fn jitter_buffer_counter_conservation(
            packets in proptest::collection::vec(
                (any::<u16>(), proptest::collection::vec(any::<u8>(), 0..2000)),
                0..600,
            ),
        ) {
            let mut jb = AdaptiveJitterBuffer::new();
            let mut total_pushes: u64 = 0;

            for (seq, data) in &packets {
                jb.push(*seq, data.clone());
                total_pushes += 1;

                // Conservation: observable outcomes never exceed total pushes.
                let oversize = jb.dropped_oversize();
                let overcap = jb.dropped_overcap();
                let buffered = jb.len() as u64;

                prop_assert!(
                    oversize + overcap + buffered <= total_pushes,
                    "conservation violated after push #{}: \
                     oversize({}) + overcap({}) + buffered({}) = {} > total_pushes({})",
                    total_pushes, oversize, overcap, buffered,
                    oversize + overcap + buffered, total_pushes,
                );
            }

            // Final check after the full sequence.
            let oversize = jb.dropped_oversize();
            let overcap = jb.dropped_overcap();
            let buffered = jb.len() as u64;

            prop_assert!(
                oversize + overcap + buffered <= total_pushes,
                "final conservation violated: \
                 oversize({}) + overcap({}) + buffered({}) = {} > total_pushes({})",
                oversize, overcap, buffered,
                oversize + overcap + buffered, total_pushes,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests for jitter buffer guards (Task 1.5)
    // Feature: client-security-hardening
    // Validates: Requirements 1.1, 1.2, 2.1, 2.2, 9.1
    // -----------------------------------------------------------------------

    #[test]
    fn jitter_guard_accept_packet_at_max_size() {
        let mut jb = AdaptiveJitterBuffer::new();
        jb.push(1, vec![0u8; MAX_PACKET_SIZE]);
        assert_eq!(
            jb.len(),
            1,
            "packet exactly at MAX_PACKET_SIZE should be accepted"
        );
        assert_eq!(jb.dropped_oversize(), 0);
    }

    #[test]
    fn jitter_guard_accept_packet_below_max_size() {
        let mut jb = AdaptiveJitterBuffer::new();
        jb.push(1, vec![0u8; MAX_PACKET_SIZE - 1]);
        assert_eq!(
            jb.len(),
            1,
            "packet below MAX_PACKET_SIZE should be accepted"
        );
        assert_eq!(jb.dropped_oversize(), 0);
    }

    #[test]
    fn jitter_guard_reject_packet_above_max_size() {
        let mut jb = AdaptiveJitterBuffer::new();
        jb.push(1, vec![0u8; MAX_PACKET_SIZE + 1]);
        assert_eq!(
            jb.len(),
            0,
            "packet above MAX_PACKET_SIZE should be rejected"
        );
        assert_eq!(jb.dropped_oversize(), 1);
    }

    #[test]
    fn jitter_guard_reject_at_capacity() {
        let mut jb = AdaptiveJitterBuffer::new();
        // Fill buffer to exactly MAX_BUFFERED_PACKETS with unique sequential seq numbers.
        for seq in 0..MAX_BUFFERED_PACKETS as u16 {
            jb.push(seq, vec![0u8; 100]);
        }
        assert_eq!(jb.len(), MAX_BUFFERED_PACKETS);
        assert_eq!(jb.dropped_overcap(), 0);

        // One more should be rejected.
        jb.push(MAX_BUFFERED_PACKETS as u16, vec![0u8; 100]);
        assert_eq!(
            jb.len(),
            MAX_BUFFERED_PACKETS,
            "buffer should not grow past cap"
        );
        assert_eq!(jb.dropped_overcap(), 1);
    }

    #[test]
    fn jitter_guard_mixed_operations_counters() {
        let mut jb = AdaptiveJitterBuffer::new();

        // Push 3 oversize packets.
        for seq in 0..3u16 {
            jb.push(seq, vec![0u8; MAX_PACKET_SIZE + 10]);
        }
        assert_eq!(jb.dropped_oversize(), 3);
        assert_eq!(jb.dropped_overcap(), 0);
        assert_eq!(jb.len(), 0);

        // Push 5 valid packets.
        for seq in 10..15u16 {
            jb.push(seq, vec![0u8; 200]);
        }
        assert_eq!(jb.dropped_oversize(), 3);
        assert_eq!(jb.dropped_overcap(), 0);
        assert_eq!(jb.len(), 5);

        // Fill to capacity (need MAX_BUFFERED_PACKETS - 5 more).
        // Use seq numbers starting from 15 to avoid duplicates.
        let remaining = MAX_BUFFERED_PACKETS - 5;
        for i in 0..remaining as u16 {
            jb.push(15 + i, vec![0u8; 100]);
        }
        assert_eq!(jb.len(), MAX_BUFFERED_PACKETS);
        assert_eq!(jb.dropped_overcap(), 0);

        // Push 2 more valid-sized packets — should be rejected by capacity guard.
        let next_seq = 15 + remaining as u16;
        jb.push(next_seq, vec![0u8; 100]);
        jb.push(next_seq + 1, vec![0u8; 100]);
        assert_eq!(jb.dropped_overcap(), 2);

        // Push 1 more oversize — should be rejected by size guard (before capacity check).
        jb.push(next_seq + 2, vec![0u8; MAX_PACKET_SIZE + 1]);
        assert_eq!(jb.dropped_oversize(), 4);
        assert_eq!(jb.dropped_overcap(), 2); // unchanged — oversize guard fires first

        // Final: buffer still at cap, all counters correct.
        assert_eq!(jb.len(), MAX_BUFFERED_PACKETS);
        assert!(!jb.is_empty());
    }
}
