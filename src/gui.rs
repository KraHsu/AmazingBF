//! Tauri GUI bootstrap for the `bf-gui` binary (gated on feature `gui`).
//!
//! Hosts the interpreter in a worker thread, owns the Tauri main thread, and
//! wires both sides to a shared `GuiShared` / `GuiIo` pair. Pixels flow out as
//! coalesced frame messages over a `tauri::ipc::Channel`; keypresses flow in
//! via the `send_key` IPC command.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Builder, State};

use crate::driver::config::{
    CompileTarget, DEFAULT_INTERPRETER_TAPE_LEN, DriverConfig, OptLevel, RunMode,
};
use crate::driver::pipeline::build_frontend;
use crate::interp::engine::Interpreter;
use crate::runtime::gui_io::GuiIo;
use crate::runtime::gui_io::GuiShared;
use crate::runtime::host::NullHost;

/// Depth-1 bounded channel: if the forwarder is already holding a frame,
/// the producer drops the new one and will republish with the latest bbox
/// on the next publish tick. This coalesces backpressure automatically.
const FRAME_CHANNEL_DEPTH: usize = 1;

#[tauri::command]
fn send_key(key: u8, shared: State<Arc<GuiShared>>) {
    {
        let mut q = shared.keys.lock().unwrap();
        q.push_back(key);
    }
    shared.cv_key.notify_one();
}

#[tauri::command]
fn subscribe_frames(channel: Channel<InvokeResponseBody>, shared: State<Arc<GuiShared>>) {
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(FRAME_CHANNEL_DEPTH);

    // Install the new sender; dropping the previous one closes the prior
    // forwarder thread's receiver loop, letting it exit cleanly.
    {
        let mut slot = shared.frame_tx.lock().unwrap();
        *slot = Some(tx);
    }

    thread::spawn(move || {
        for bytes in rx {
            if channel.send(InvokeResponseBody::Raw(bytes)).is_err() {
                // Webview gone — stop forwarding. The producer keeps trying
                // to send; try_send returning Disconnected is a no-op.
                break;
            }
        }
    });
}

fn parse_opt_level(args: &[String]) -> OptLevel {
    args.windows(2)
        .find(|w| w[0] == "-O")
        .and_then(|w| OptLevel::parse(&w[1]))
        .unwrap_or(OptLevel::O1)
}

/// Entry point of the `bf-gui` binary: parse CLI args, build HIR, spawn the
/// interpreter thread, and hand the main thread to the Tauri event loop.
pub(crate) fn run() -> crate::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        return Err(crate::error::Error::Other(
            "usage: bf-gui <file.bf> [-O <0-3>]".into(),
        ));
    }

    let raw = std::fs::read_to_string(&args[1])
        .map_err(|e| crate::error::Error::Other(format!("failed to read {}: {e}", args[1])))?;

    let source = if args[1].ends_with(".bfs") {
        crate::bfsc::compile(&raw).map_err(|e| crate::error::Error::Other(format!("bfsc: {e}")))?
    } else {
        raw
    };

    let config = DriverConfig {
        source,
        mode: RunMode::Interpret,
        target: CompileTarget::build_default(),
        output: Default::default(),
        interp_debug: false,
        opt_level: parse_opt_level(&args),
        #[cfg(target_os = "linux")]
        jit_threshold: None,
    };

    let frontend = build_frontend(&config)?;
    let hir = frontend.hir;

    let shared = GuiShared::new();

    let shared_thr = shared.clone();
    thread::spawn(move || {
        let io = GuiIo::new(shared_thr);
        let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, io, NullHost::new());
        if let Err(e) = interp.run(&hir) {
            eprintln!("interpreter error: {e}");
        }
    });

    Builder::default()
        .manage(shared)
        .invoke_handler(tauri::generate_handler![send_key, subscribe_frames])
        .run(tauri::generate_context!())
        .map_err(|e| crate::error::Error::Other(e.to_string()))
}
