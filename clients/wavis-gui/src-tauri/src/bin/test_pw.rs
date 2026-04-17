//! PipeWire + WebKitGTK test to debug MainLoop crash in Tauri.
//! Run: cargo run --bin test_pw -p wavis-gui
//!
//! Test 1 (passed): GTK init + PipeWire on thread → works fine.
//! Test 2 (passed): GTK main loop running + PipeWire on thread → works fine.
//! Test 3 (passed): WebKitGTK webview + PipeWire on thread → works fine.
//! Test 4: WITHOUT pw::init() + WebKitGTK + PipeWire on thread → crash?

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("test_pw is only available on Linux");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
use gtk::glib;
#[cfg(target_os = "linux")]
use webkit2gtk::{WebView, WebViewExt};

#[cfg(target_os = "linux")]
fn main() {
    eprintln!("=== Test 4: WebKitGTK + PipeWire WITHOUT pw::init() ===");

    eprintln!("[1] initializing GTK");
    gtk::init().expect("GTK init failed");
    eprintln!("[2] GTK initialized");

    // NOTE: NOT calling pipewire::init() — simulates the real code
    eprintln!("[3] SKIPPING pipewire::init()");

    eprintln!("[4] creating WebKitGTK WebView");
    let webview = WebView::new();
    webview.load_uri("about:blank");
    eprintln!("[5] WebView created and loaded");

    let (tx, rx) = std::sync::mpsc::channel();

    let handle = std::thread::Builder::new()
        .name("pw-capture".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));

            eprintln!("[6] spawned thread: creating MainLoop (no pw::init)");
            let mainloop = match pipewire::main_loop::MainLoopRc::new(None) {
                Ok(ml) => {
                    eprintln!("[7] MainLoop created");
                    ml
                }
                Err(e) => {
                    eprintln!("[7] MainLoop FAILED: {e}");
                    let _ = tx.send(());
                    return;
                }
            };
            let context = match pipewire::context::ContextRc::new(&mainloop, None) {
                Ok(ctx) => {
                    eprintln!("[8] Context created");
                    ctx
                }
                Err(e) => {
                    eprintln!("[8] Context FAILED: {e}");
                    let _ = tx.send(());
                    return;
                }
            };
            let core = match context.connect_rc(None) {
                Ok(c) => {
                    eprintln!("[9] connected");
                    c
                }
                Err(e) => {
                    eprintln!("[9] connect FAILED: {e}");
                    let _ = tx.send(());
                    return;
                }
            };
            let _registry = match core.get_registry() {
                Ok(r) => {
                    eprintln!("[10] registry OK");
                    r
                }
                Err(e) => {
                    eprintln!("[10] registry FAILED: {e}");
                    let _ = tx.send(());
                    return;
                }
            };
            eprintln!("[11] PipeWire all good without pw::init()!");
            let _ = tx.send(());
        })
        .unwrap();

    glib::idle_add_local(move || {
        if rx.try_recv().is_ok() {
            eprintln!("[12] GTK main loop quitting");
            gtk::main_quit();
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });

    let _wv = webview;
    eprintln!("[GTK] entering main loop");
    gtk::main();
    eprintln!("[GTK] main loop exited");

    handle.join().unwrap();
    eprintln!("=== Test complete ===");
}
