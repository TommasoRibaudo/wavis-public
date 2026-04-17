use super::super::audio_capture_state::AudioShareStartResult;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VirtualDeviceCandidate {
    pub(super) device_id: u32,
    pub(super) uid: String,
    pub(super) name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StaleCleanupDeviceInfo {
    pub(super) device_id: u32,
    pub(super) name: Option<String>,
    pub(super) uid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StaleMultiOutputCleanupPlan {
    pub(super) replacement_default_output: Option<u32>,
    pub(super) stale_bridge_ids: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MacAudioShareDecision {
    Tap,
    VirtualDevice,
    ScreenCaptureKit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VirtualDeviceTeardownTrigger {
    ManualStop,
    SessionDisconnect,
    CaptureError,
    Drop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VirtualDeviceTeardownSnapshot {
    pub(super) original_default_output: u32,
    pub(super) aggregate_device_id: u32,
    pub(super) audio_queue_registered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VirtualDeviceTeardownAction {
    RestoreDefaultOutput(u32),
    StopAudioQueue,
    DestroyAudioQueue,
    DestroyAggregateDevice(u32),
}

const WAVIS_AUDIO_BRIDGE_NAME: &str = "Wavis Audio Bridge";
const WAVIS_AUDIO_BRIDGE_UID_PREFIX: &str = "com.wavis.audio-bridge-";

pub(super) fn bare_sck_fallback_result() -> AudioShareStartResult {
    AudioShareStartResult {
        loopback_exclusion_available: false,
        real_output_device_id: None,
        real_output_device_name: None,
        requires_mute_for_echo_prevention: false,
    }
}

pub(super) fn select_macos_audio_share_decision(
    tap_result: Option<AudioShareStartResult>,
    virtual_device_result: Option<AudioShareStartResult>,
) -> (MacAudioShareDecision, AudioShareStartResult) {
    if let Some(result) = tap_result {
        (MacAudioShareDecision::Tap, result)
    } else if let Some(result) = virtual_device_result {
        (MacAudioShareDecision::VirtualDevice, result)
    } else {
        (
            MacAudioShareDecision::ScreenCaptureKit,
            bare_sck_fallback_result(),
        )
    }
}

pub(super) fn virtual_device_rank(name: &str) -> Option<u8> {
    let normalized = name.to_ascii_lowercase();
    if normalized.contains("wavis audio tap") {
        Some(0) // bundled driver — highest priority
    } else if normalized.contains("blackhole 2ch") {
        Some(1)
    } else if normalized.contains("blackhole 16ch") {
        Some(2)
    } else if normalized.contains("blackhole") || normalized.contains("loopback") {
        Some(3)
    } else if let Ok(override_name) = std::env::var("WAVIS_LOOPBACK_DEVICE") {
        // Configurable fallback for non-standard loopback device names.
        // Set WAVIS_LOOPBACK_DEVICE to a substring of the device name to match.
        if !override_name.is_empty() && normalized.contains(&override_name.to_ascii_lowercase()) {
            Some(4)
        } else {
            None
        }
    } else {
        None
    }
}

pub(super) fn select_virtual_device_candidate<I>(candidates: I) -> Option<VirtualDeviceCandidate>
where
    I: IntoIterator<Item = VirtualDeviceCandidate>,
{
    let mut best_match: Option<(u8, usize, VirtualDeviceCandidate)> = None;

    for (index, candidate) in candidates.into_iter().enumerate() {
        let Some(rank) = virtual_device_rank(&candidate.name) else {
            continue;
        };

        let should_replace = best_match
            .as_ref()
            .map(|(best_rank, best_index, _)| (rank, index) < (*best_rank, *best_index))
            .unwrap_or(true);

        if should_replace {
            best_match = Some((rank, index, candidate));
        }
    }

    best_match.map(|(_, _, candidate)| candidate)
}

fn is_wavis_audio_bridge_name(name: &str) -> bool {
    name == WAVIS_AUDIO_BRIDGE_NAME
}

fn is_wavis_audio_bridge_uid(uid: &str) -> bool {
    uid.starts_with(WAVIS_AUDIO_BRIDGE_UID_PREFIX)
}

/// Returns true when a device is a Wavis Audio Bridge, matched by name OR UID
/// prefix. The UID check catches devices that CoreAudio may have renamed (e.g.
/// "Wavis Audio Bridge 1") while keeping the programmatic UID we assigned.
fn is_wavis_bridge_device(device: &StaleCleanupDeviceInfo) -> bool {
    device
        .name
        .as_deref()
        .map(is_wavis_audio_bridge_name)
        .unwrap_or(false)
        || device
            .uid
            .as_deref()
            .map(is_wavis_audio_bridge_uid)
            .unwrap_or(false)
}

fn is_real_output_candidate(device: &StaleCleanupDeviceInfo, excluded_ids: &[u32]) -> bool {
    if excluded_ids.contains(&device.device_id) {
        return false;
    }
    if is_wavis_bridge_device(device) {
        return false;
    }
    let Some(name) = device.name.as_deref() else {
        return false;
    };
    if virtual_device_rank(name).is_some() {
        return false;
    }
    matches!(device.uid.as_deref(), Some(uid) if !uid.is_empty())
}

pub(super) fn plan_stale_multi_output_cleanup(
    devices: &[StaleCleanupDeviceInfo],
    default_output_device_id: u32,
) -> Result<Option<StaleMultiOutputCleanupPlan>, String> {
    let device_ids: Vec<u32> = devices.iter().map(|device| device.device_id).collect();
    // Match by name OR UID prefix so renamed bridges don't slip through.
    let stale_bridge_ids: Vec<u32> = devices
        .iter()
        .filter(|device| is_wavis_bridge_device(device))
        .map(|device| device.device_id)
        .collect();

    if stale_bridge_ids.is_empty() && device_ids.contains(&default_output_device_id) {
        return Ok(None);
    }

    let default_needs_reset = stale_bridge_ids.contains(&default_output_device_id)
        || !device_ids.contains(&default_output_device_id);
    let replacement_default_output = if default_needs_reset {
        Some(
            devices
                .iter()
                .find(|device| is_real_output_candidate(device, &stale_bridge_ids))
                .map(|device| device.device_id)
                .ok_or_else(|| {
                    format!(
                    "default output device {} is stale/invalid and no replacement output device \
                     was available",
                    default_output_device_id
                )
                })?,
        )
    } else {
        None
    };

    Ok(Some(StaleMultiOutputCleanupPlan {
        replacement_default_output,
        stale_bridge_ids,
    }))
}

pub(super) fn plan_virtual_device_teardown(
    _trigger: VirtualDeviceTeardownTrigger,
    snapshot: VirtualDeviceTeardownSnapshot,
) -> Vec<VirtualDeviceTeardownAction> {
    let mut actions = Vec::new();

    if snapshot.original_default_output != 0 {
        actions.push(VirtualDeviceTeardownAction::RestoreDefaultOutput(
            snapshot.original_default_output,
        ));
    }

    if snapshot.audio_queue_registered {
        actions.push(VirtualDeviceTeardownAction::StopAudioQueue);
        actions.push(VirtualDeviceTeardownAction::DestroyAudioQueue);
    }

    if snapshot.aggregate_device_id != 0 {
        actions.push(VirtualDeviceTeardownAction::DestroyAggregateDevice(
            snapshot.aggregate_device_id,
        ));
    }

    actions
}

#[cfg(test)]
mod tests {
    use crate::audio_capture::{
        audio_capture_state::AudioShareStartResult,
        proptest_support::{
            arb_macos_fallback_case, arb_output_device_id, arb_share_lifecycle_ops,
            ShareLifecycleOp,
        },
    };
    use proptest::prelude::*;

    use super::{
        bare_sck_fallback_result, plan_stale_multi_output_cleanup, plan_virtual_device_teardown,
        select_macos_audio_share_decision, select_virtual_device_candidate, virtual_device_rank,
        MacAudioShareDecision, StaleCleanupDeviceInfo, VirtualDeviceCandidate,
        VirtualDeviceTeardownAction, VirtualDeviceTeardownSnapshot, VirtualDeviceTeardownTrigger,
    };
    use crate::audio_capture::platform::try_start_process_tap;

    #[derive(Clone, Copy, Debug)]
    struct SimulatedVirtualShareSession {
        original_default_output: u32,
        aggregate_device_id: u32,
    }

    #[derive(Debug)]
    struct RoutingCleanupSimulation {
        initial_default_output: u32,
        current_default_output: u32,
        next_device_id: u32,
        active_session: Option<SimulatedVirtualShareSession>,
    }

    impl RoutingCleanupSimulation {
        fn new(initial_default_output: u32) -> Self {
            Self {
                initial_default_output,
                current_default_output: initial_default_output,
                next_device_id: initial_default_output.saturating_add(1).max(2),
                active_session: None,
            }
        }

        fn apply(&mut self, op: ShareLifecycleOp) {
            match op {
                ShareLifecycleOp::Start => self.start(),
                ShareLifecycleOp::Stop => {
                    self.finish(VirtualDeviceTeardownTrigger::ManualStop);
                }
                ShareLifecycleOp::Crash => {
                    self.finish(VirtualDeviceTeardownTrigger::Drop);
                }
            }
        }

        fn finalize(&mut self) {
            self.finish(VirtualDeviceTeardownTrigger::Drop);
        }

        fn start(&mut self) {
            if self.active_session.is_some() {
                return;
            }

            let session = SimulatedVirtualShareSession {
                original_default_output: self.current_default_output,
                aggregate_device_id: self.next_device_id,
            };
            self.next_device_id = self.next_device_id.saturating_add(1);
            self.current_default_output = session.aggregate_device_id;
            self.active_session = Some(session);
        }

        fn finish(&mut self, trigger: VirtualDeviceTeardownTrigger) {
            let Some(session) = self.active_session.take() else {
                return;
            };

            let actions = plan_virtual_device_teardown(
                trigger,
                VirtualDeviceTeardownSnapshot {
                    original_default_output: session.original_default_output,
                    aggregate_device_id: session.aggregate_device_id,
                    audio_queue_registered: true,
                },
            );

            for action in actions {
                match action {
                    VirtualDeviceTeardownAction::RestoreDefaultOutput(device_id) => {
                        self.current_default_output = device_id;
                    }
                    VirtualDeviceTeardownAction::StopAudioQueue
                    | VirtualDeviceTeardownAction::DestroyAudioQueue => {
                        // Queue teardown is validated by ordering (stop before dispose);
                        // the queue ref itself lives in routing state, not the snapshot.
                    }
                    VirtualDeviceTeardownAction::DestroyAggregateDevice(device_id) => {
                        assert_eq!(
                            device_id, session.aggregate_device_id,
                            "teardown must destroy the active aggregate device"
                        );
                    }
                }
            }

            assert_eq!(
                self.current_default_output, session.original_default_output,
                "teardown must restore the pre-share default output before the session ends"
            );
        }
    }

    fn candidate(device_id: u32, uid: &str, name: &str) -> VirtualDeviceCandidate {
        VirtualDeviceCandidate {
            device_id,
            uid: uid.to_string(),
            name: name.to_string(),
        }
    }

    fn cleanup_device(
        device_id: u32,
        name: Option<&str>,
        uid: Option<&str>,
    ) -> StaleCleanupDeviceInfo {
        StaleCleanupDeviceInfo {
            device_id,
            name: name.map(str::to_string),
            uid: uid.map(str::to_string),
        }
    }

    fn tap_result() -> AudioShareStartResult {
        AudioShareStartResult {
            loopback_exclusion_available: true,
            real_output_device_id: None,
            real_output_device_name: None,
            requires_mute_for_echo_prevention: false,
        }
    }

    fn virtual_device_result() -> AudioShareStartResult {
        AudioShareStartResult {
            loopback_exclusion_available: true,
            real_output_device_id: Some("built-in-output".to_string()),
            real_output_device_name: None,
            requires_mute_for_echo_prevention: false,
        }
    }

    #[test]
    fn virtual_device_detection_matches_supported_substrings() {
        assert_eq!(virtual_device_rank("Wavis Audio Tap"), Some(0));
        assert_eq!(virtual_device_rank("BlackHole 2ch"), Some(1));
        assert_eq!(virtual_device_rank("BlackHole 16ch"), Some(2));
        assert_eq!(virtual_device_rank("Loopback Audio"), Some(3));
        assert_eq!(virtual_device_rank("blackhole custom"), Some(3));
    }

    #[test]
    fn virtual_device_detection_returns_none_when_no_supported_device_exists() {
        let selected = select_virtual_device_candidate([
            candidate(1, "speaker", "MacBook Pro Speakers"),
            candidate(2, "headset", "USB Headset"),
        ]);

        assert_eq!(selected, None);
    }

    #[test]
    fn virtual_device_detection_prefers_wavis_tap_over_blackhole_over_loopback() {
        let selected = select_virtual_device_candidate([
            candidate(10, "loopback", "Loopback Audio"),
            candidate(11, "blackhole-16", "BlackHole 16ch"),
            candidate(12, "blackhole-2", "BlackHole 2ch"),
            candidate(13, "wavis-tap", "Wavis Audio Tap"),
        ]);

        assert_eq!(
            selected,
            Some(candidate(13, "wavis-tap", "Wavis Audio Tap"))
        );
    }

    #[test]
    fn virtual_device_detection_prefers_blackhole_2ch_then_16ch_then_other_loopback() {
        let selected = select_virtual_device_candidate([
            candidate(10, "loopback", "Loopback Audio"),
            candidate(11, "blackhole-16", "BlackHole 16ch"),
            candidate(12, "blackhole-2", "BlackHole 2ch"),
        ]);

        assert_eq!(
            selected,
            Some(candidate(12, "blackhole-2", "BlackHole 2ch"))
        );
    }

    #[test]
    fn three_tier_fallback_prefers_process_tap_when_available() {
        let (decision, result) =
            select_macos_audio_share_decision(Some(tap_result()), Some(virtual_device_result()));

        assert_eq!(decision, MacAudioShareDecision::Tap);
        assert_eq!(result, tap_result());
    }

    #[test]
    fn three_tier_fallback_uses_virtual_device_when_tap_is_unavailable() {
        let (decision, result) =
            select_macos_audio_share_decision(None, Some(virtual_device_result()));

        assert_eq!(decision, MacAudioShareDecision::VirtualDevice);
        assert_eq!(result, virtual_device_result());
    }

    #[test]
    fn three_tier_fallback_uses_bare_sck_when_isolation_is_unavailable() {
        let (decision, result) = select_macos_audio_share_decision(None, None);

        assert_eq!(decision, MacAudioShareDecision::ScreenCaptureKit);
        assert_eq!(result, bare_sck_fallback_result());
    }

    #[test]
    fn loopback_exclusion_semantics_match_selected_tier() {
        let (_, tap) = select_macos_audio_share_decision(Some(tap_result()), None);
        let (_, virtual_device) =
            select_macos_audio_share_decision(None, Some(virtual_device_result()));
        let (_, sck) = select_macos_audio_share_decision(None, None);

        assert!(tap.loopback_exclusion_available);
        assert!(virtual_device.loopback_exclusion_available);
        assert!(!sck.loopback_exclusion_available);
    }

    #[test]
    fn teardown_plan_matches_manual_stop_disconnect_and_capture_error_paths() {
        let snapshot = VirtualDeviceTeardownSnapshot {
            original_default_output: 100,
            aggregate_device_id: 200,
            audio_queue_registered: true,
        };
        let expected = vec![
            VirtualDeviceTeardownAction::RestoreDefaultOutput(100),
            VirtualDeviceTeardownAction::StopAudioQueue,
            VirtualDeviceTeardownAction::DestroyAudioQueue,
            VirtualDeviceTeardownAction::DestroyAggregateDevice(200),
        ];

        assert_eq!(
            plan_virtual_device_teardown(VirtualDeviceTeardownTrigger::ManualStop, snapshot),
            expected
        );
        assert_eq!(
            plan_virtual_device_teardown(VirtualDeviceTeardownTrigger::SessionDisconnect, snapshot),
            expected
        );
        assert_eq!(
            plan_virtual_device_teardown(VirtualDeviceTeardownTrigger::CaptureError, snapshot),
            expected
        );
    }

    #[test]
    fn drop_cleanup_plan_attempts_restoration_for_populated_routing_state() {
        let actions = plan_virtual_device_teardown(
            VirtualDeviceTeardownTrigger::Drop,
            VirtualDeviceTeardownSnapshot {
                original_default_output: 10,
                aggregate_device_id: 20,
                audio_queue_registered: true,
            },
        );

        assert!(
            actions.contains(&VirtualDeviceTeardownAction::RestoreDefaultOutput(10)),
            "drop cleanup must attempt to restore the original system default output"
        );
        assert!(
            actions.contains(&VirtualDeviceTeardownAction::DestroyAggregateDevice(20)),
            "drop cleanup must attempt to destroy the temporary multi-output device"
        );
    }

    #[test]
    fn stale_cleanup_plans_bridge_destruction_and_default_output_reset() {
        let plan = plan_stale_multi_output_cleanup(
            &[
                cleanup_device(1, Some("Wavis Audio Bridge"), Some("bridge-1")),
                cleanup_device(2, Some("Built-in Output"), Some("built-in-output")),
                cleanup_device(3, Some("BlackHole 2ch"), Some("blackhole-2ch")),
            ],
            1,
        )
        .expect("stale cleanup plan should be produced")
        .expect("stale bridge should require cleanup");

        assert_eq!(plan.replacement_default_output, Some(2));
        assert_eq!(plan.stale_bridge_ids, vec![1]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn random_start_stop_crash_sequences_restore_the_original_default_output(
            initial_default_output in arb_output_device_id(),
            operations in arb_share_lifecycle_ops(),
        ) {
            let mut simulation = RoutingCleanupSimulation::new(initial_default_output);

            for operation in operations {
                simulation.apply(operation);
            }
            simulation.finalize();

            prop_assert_eq!(
                simulation.current_default_output,
                simulation.initial_default_output,
                "all simulated share lifecycles must restore the original default output device"
            );
            prop_assert!(
                simulation.active_session.is_none(),
                "no virtual-device session should remain active after final cleanup"
            );
        }

        #[test]
        fn random_macos_fallback_inputs_select_the_expected_capture_tier(
            (version, tap_available, virtual_device_installed) in arb_macos_fallback_case(),
        ) {
            let tap_result = try_start_process_tap(version, || {
                if tap_available {
                    Ok(tap_result())
                } else {
                    Err("AudioHardwareCreateProcessTap failed: synthetic test failure".to_string())
                }
            })
            .expect("tap helper should degrade cleanly during routing selection tests");

            let virtual_device_result = if tap_result.is_none() && virtual_device_installed {
                Some(virtual_device_result())
            } else {
                None
            };
            let (decision, result) =
                select_macos_audio_share_decision(tap_result, virtual_device_result);

            let expected_decision = if version.supports_process_tap() && tap_available {
                MacAudioShareDecision::Tap
            } else if virtual_device_installed {
                MacAudioShareDecision::VirtualDevice
            } else {
                MacAudioShareDecision::ScreenCaptureKit
            };

            prop_assert_eq!(decision, expected_decision);

            match expected_decision {
                MacAudioShareDecision::Tap => {
                    prop_assert!(result.loopback_exclusion_available);
                    prop_assert_eq!(result.real_output_device_id, None);
                }
                MacAudioShareDecision::VirtualDevice => {
                    prop_assert!(result.loopback_exclusion_available);
                    prop_assert_eq!(
                        result.real_output_device_id,
                        Some("built-in-output".to_string())
                    );
                }
                MacAudioShareDecision::ScreenCaptureKit => {
                    prop_assert!(!result.loopback_exclusion_available);
                    prop_assert_eq!(result.real_output_device_id, None);
                }
            }
        }
    }
}
