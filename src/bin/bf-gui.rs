//! `bf-gui` binary: Brainfuck interpreter driving a Tauri webview.
//!
//! Sets Linux-specific WebKit workarounds (force XWayland + software renderer)
//! by re-exec'ing itself with the environment tweaked, then hands control to
//! [`AmazingBF::run_bf_gui`].

fn main() {
    // WebKit on Linux can fail with Wayland protocol errors or GBM buffer errors
    // depending on the compositor. Force XWayland + software rendering on first launch.
    if std::env::var_os("_BF_GUI_REEXEC").is_none() {
        let exe = std::env::current_exe().expect("cannot resolve own exe path");
        let status = std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env("_BF_GUI_REEXEC", "1")
            .env("GDK_BACKEND", "x11")
            .env("WEBKIT_DISABLE_COMPOSITING_MODE", "1")
            .env("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
            .status()
            .expect("failed to re-exec with WebKit workarounds");
        std::process::exit(status.code().unwrap_or(1));
    }

    if let Err(e) = AmazingBF::run_bf_gui() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
