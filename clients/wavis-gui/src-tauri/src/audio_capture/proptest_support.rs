use super::platform::MacOsVersion;
use proptest::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::audio_capture) enum ShareLifecycleOp {
    Start,
    Stop,
    Crash,
}

pub(in crate::audio_capture) fn arb_output_device_id() -> impl Strategy<Value = u32> {
    1u32..=4096
}

pub(in crate::audio_capture) fn arb_share_lifecycle_ops(
) -> impl Strategy<Value = Vec<ShareLifecycleOp>> {
    proptest::collection::vec(
        prop_oneof![
            Just(ShareLifecycleOp::Start),
            Just(ShareLifecycleOp::Stop),
            Just(ShareLifecycleOp::Crash),
        ],
        1..=32,
    )
}

pub(in crate::audio_capture) fn arb_macos_version() -> impl Strategy<Value = MacOsVersion> {
    (11isize..=20, 0isize..=9, 0isize..=9).prop_map(|(major, minor, patch)| MacOsVersion {
        major,
        minor,
        patch,
    })
}

pub(in crate::audio_capture) fn arb_process_tap_supported_version(
) -> impl Strategy<Value = MacOsVersion> {
    arb_macos_version().prop_filter(
        "macOS 14.2+ should attempt the process tap path",
        |version| version.supports_process_tap(),
    )
}

pub(in crate::audio_capture) fn arb_process_tap_unsupported_version(
) -> impl Strategy<Value = MacOsVersion> {
    arb_macos_version().prop_filter(
        "pre-14.2 should skip the process tap path entirely",
        |version| !version.supports_process_tap(),
    )
}

pub(in crate::audio_capture) fn arb_macos_fallback_case(
) -> impl Strategy<Value = (MacOsVersion, bool, bool)> {
    (arb_macos_version(), any::<bool>(), any::<bool>())
}
