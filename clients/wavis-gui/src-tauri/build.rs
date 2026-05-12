fn main() {
    tauri_build::build();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // ScreenCaptureKit was introduced in macOS 12.3. Weak-link so the app
        // starts on older macOS; the runtime check in audio_capture prevents any
        // SCK code from running on < 12.3.
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");

        // CGPreflightScreenCaptureAccess / CGRequestScreenCaptureAccess were
        // added in macOS 10.15. Weak-link the symbols so they resolve to NULL
        // on older macOS; ensure_screen_recording_access() checks for NULL and
        // returns "authorized" (no Screen Recording permission system existed
        // before Catalina — screen capture was always allowed).
        println!("cargo:rustc-link-arg=-Wl,-U,_CGPreflightScreenCaptureAccess");
        println!("cargo:rustc-link-arg=-Wl,-U,_CGRequestScreenCaptureAccess");
    }
}
