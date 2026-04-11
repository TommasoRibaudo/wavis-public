fn main() {
    // Weak-link ScreenCaptureKit so the binary loads on macOS < 12.3.
    // The runtime version check in audio_capture/platform/macos.rs gates
    // actual usage — on older systems the feature is simply unavailable.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");

    tauri_build::build()
}
