use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tauri::{Builder, State};

use crate::driver::config::{CompileTarget, DEFAULT_INTERPRETER_TAPE_LEN, DriverConfig, OptLevel, RunMode};
use crate::driver::pipeline::build_frontend;
use crate::interp::engine::Interpreter;
use crate::runtime::gui_io::{GuiIo, KeyQueue, ScreenBuf, SCREEN_CELLS};
use crate::runtime::host::NullHost;

#[tauri::command]
fn get_screen(screen: State<ScreenBuf>) -> Vec<u8> {
    screen.inner().lock().unwrap().clone()
}

#[tauri::command]
fn send_key(key: u8, keys: State<KeyQueue>) {
    keys.inner().lock().unwrap().push_back(key);
}

fn parse_opt_level(args: &[String]) -> OptLevel {
    args.windows(2)
        .find(|w| w[0] == "-O")
        .and_then(|w| OptLevel::parse(&w[1]))
        .unwrap_or(OptLevel::O1)
}

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
        crate::bfsc::compile(&raw)
            .map_err(|e| crate::error::Error::Other(format!("bfsc: {e}")))?
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
    };

    let frontend = build_frontend(&config)?;
    let hir = frontend.hir;

    let screen: ScreenBuf = Arc::new(Mutex::new(vec![0u8; SCREEN_CELLS]));
    let keys: KeyQueue = Arc::new(Mutex::new(VecDeque::new()));

    let screen_thr = screen.clone();
    let keys_thr = keys.clone();
    std::thread::spawn(move || {
        let io = GuiIo::new(screen_thr, keys_thr);
        let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, io, NullHost::new());
        if let Err(e) = interp.run(&hir) {
            eprintln!("interpreter error: {e}");
        }
    });

    Builder::default()
        .manage(screen)
        .manage(keys)
        .invoke_handler(tauri::generate_handler![get_screen, send_key])
        .run(tauri::generate_context!())
        .map_err(|e| crate::error::Error::Other(e.to_string()))
}
