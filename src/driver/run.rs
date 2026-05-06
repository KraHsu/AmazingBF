//! Mode dispatch: interpret / compile / dump, after the frontend has produced HIR.
//!
//! `run()` accepts a fully-validated `DriverConfig`, invokes the shared frontend,
//! then routes HIR into the interpreter, into LIR→asm→ELF/PE64 codegen, or
//! simply logs pipeline sizes without writing any artifact.

use crate::Result;
use crate::backend::asm::AsmProgram;
use crate::backend::codegen::{
    compile_lir_to_asm, compile_lir_to_jit_asm, compile_precomputed_stdout_asm,
    compile_trivial_exit_asm,
};
use crate::backend::x86_64::debug;
use crate::backend::x86_64::relax::relax_jumps;
use crate::backend::x86_64::windows::{
    WindowsProgram, compile_lir_to_windows_program, compile_precomputed_stdout_program,
    compile_trivial_exit_program,
};
use crate::backend::x86_64::{compile_asm_to_elf, compile_windows_program_to_pe};
use crate::driver::artifacts::{
    ASM_LISTING_EXT, HEX_LISTING_EXT, artifact_path, compile_executable_output_path,
    write_artifact, write_executable,
};
#[cfg(target_os = "linux")]
use crate::driver::config::DEFAULT_JIT_THRESHOLD;
use crate::driver::config::{
    CompileTarget, DEFAULT_INTERPRETER_TAPE_LEN, DriverConfig, OptLevel, RunMode,
};
use crate::driver::pipeline::build_frontend;
use crate::interp::engine::Interpreter;
use crate::interp::profile::{DEFAULT_HOTSPOT_TOP_N, format_hotspot_report};
use crate::ir::hir::HirProgram;
use crate::ir::lir::LirProgram;
use crate::ir::lir_opt::optimize_lir;
use crate::ir::lir_postpone::postpone_pointer_adds;
use crate::ir::lir_scan_hint::promote_scan_hints;
use crate::ir::lower::lower_to_lir;
use crate::logging::{log_debug, log_info};
use crate::runtime::host::NullHost;
use crate::runtime::io::{BufferOutputIo, BufferedStdIo};

/// Execute the configured pipeline after CLI / logging setup has completed.
pub(crate) fn run(config: DriverConfig) -> Result<()> {
    log_info("starting pipeline");

    let frontend = build_frontend(&config)?;
    log_debug(format!(
        "frontend pipeline completed (tokens={} ast_nodes={} hir_insts={})",
        frontend.token_count,
        frontend.ast_nodes,
        frontend.hir.insts.len()
    ));

    match config.mode {
        RunMode::Interpret => run_interpret(&config, &frontend.hir)?,
        RunMode::Compile => run_compile(&config, &frontend.hir)?,
        RunMode::Dump => run_dump(&frontend.hir, config.opt_level),
        #[cfg(target_os = "linux")]
        RunMode::Jit => run_jit(&config, &frontend.hir)?,
        #[cfg(target_os = "linux")]
        RunMode::Tiered => run_tiered(&config, &frontend.hir)?,
    }

    log_info("pipeline finished");

    Ok(())
}

fn run_interpret(config: &DriverConfig, hir: &HirProgram) -> Result<()> {
    let mut interp = Interpreter::new(
        DEFAULT_INTERPRETER_TAPE_LEN,
        BufferedStdIo::new(),
        NullHost::new(),
    );
    if config.interp_debug {
        interp.enable_hotspot_profiling(1);
    }
    interp.run(hir)?;
    log_info(format!(
        "interpreter finished (hir_insts={})",
        hir.insts.len()
    ));

    if config.interp_debug {
        emit_interp_debug_report(&interp);
    }

    Ok(())
}

fn run_compile(config: &DriverConfig, hir: &HirProgram) -> Result<()> {
    let output = compile_executable_output_path(config.target, &config.output);
    let asm_listing_path = artifact_path(&output, ASM_LISTING_EXT);
    let hex_listing_path = artifact_path(&output, HEX_LISTING_EXT);
    let (asm, executable) = match config.target {
        CompileTarget::X86_64Linux => {
            let asm = relax_jumps(compile_linux_asm(hir, config.opt_level)?);
            let executable = compile_asm_to_elf(&asm);
            (asm, executable)
        }
        CompileTarget::X86_64Windows => {
            let mut program = compile_windows_program(hir, config.opt_level)?;
            program.asm = relax_jumps(program.asm);
            let executable = compile_windows_program_to_pe(&program);
            (program.asm, executable)
        }
    };
    log_debug(format!(
        "generated asm program (asm_insts={})",
        asm.insts.len()
    ));
    write_executable(&output, &executable)?;
    write_artifact(&asm_listing_path, debug::dump_asm_listing(&asm))?;
    write_artifact(&hex_listing_path, debug::dump_hex_listing(&asm))?;

    log_info(format!(
        "compile artifacts written (target={} executable={} bytes={} asm_insts={} asm_listing={} hex_listing={})",
        config.target.as_str(),
        output.display(),
        executable.len(),
        asm.insts.len(),
        asm_listing_path.display(),
        hex_listing_path.display()
    ));

    Ok(())
}

fn run_dump(hir: &HirProgram, opt_level: OptLevel) {
    let lir = build_optimized_lir(hir, opt_level);
    log_debug(format!("lowered lir (lir_insts={})", lir.len()));
    let asm = compile_lir_to_asm(&lir);
    log_debug(format!(
        "generated asm program (asm_insts={})",
        asm.insts.len()
    ));

    log_info(format!(
        "dump mode completed without emitting files (hir_insts={} lir_insts={} asm_insts={})",
        hir.insts.len(),
        lir.len(),
        asm.insts.len()
    ));
}

fn compile_linux_asm(hir: &HirProgram, opt_level: OptLevel) -> Result<AsmProgram> {
    if opt_level == OptLevel::O3 {
        if !hir.has_put_byte() {
            log_info("compile -O3: no output ops; emitting trivial exit(0) ELF");
            return Ok(compile_trivial_exit_asm());
        }
        if !hir.has_get_byte() {
            log_info("compile -O3: no input; folding stdout to precomputed write+exit");
            let io = BufferOutputIo::default();
            let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, io, NullHost::new());
            interp.run(hir)?;
            return Ok(compile_precomputed_stdout_asm(&interp.io.bytes));
        }
    }

    let lir = build_optimized_lir(hir, opt_level);
    log_debug(format!("lowered lir (lir_insts={})", lir.len()));
    Ok(compile_lir_to_asm(&lir))
}

fn compile_windows_program(hir: &HirProgram, opt_level: OptLevel) -> Result<WindowsProgram> {
    if opt_level == OptLevel::O3 {
        if !hir.has_put_byte() {
            log_info("compile -O3: no output ops; emitting trivial exit(0) PE");
            return Ok(compile_trivial_exit_program(0));
        }
        if !hir.has_get_byte() {
            log_info("compile -O3: no input; folding stdout to precomputed WriteFile+ExitProcess");
            let io = BufferOutputIo::default();
            let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, io, NullHost::new());
            interp.run(hir)?;
            return Ok(compile_precomputed_stdout_program(&interp.io.bytes));
        }
    }

    let lir = build_optimized_lir(hir, opt_level);
    log_debug(format!("lowered lir (lir_insts={})", lir.len()));
    Ok(compile_lir_to_windows_program(&lir))
}

/// Lower HIR to LIR and run the LIR-level passes.
///
/// At `-O0` the pipeline stays a mechanical 1:1 lowering plus the basic
/// peephole fold (`PtrAdd` / `CellAdd` adjacency). From `-O1` onwards
/// [`postpone_pointer_adds`] runs before the peephole to expose
/// displacement-form writes for x86_64 codegen, and
/// [`promote_scan_hints`] lifts `Scan` to `ScanWithHint` wherever the
/// preceding bounds-check window already covers the scan traversal.
fn build_optimized_lir(hir: &HirProgram, opt_level: OptLevel) -> LirProgram {
    let lowered = lower_to_lir(hir);
    if opt_level == OptLevel::O0 {
        optimize_lir(lowered)
    } else {
        promote_scan_hints(optimize_lir(postpone_pointer_adds(lowered)))
    }
}

fn emit_interp_debug_report(interp: &Interpreter<BufferedStdIo, NullHost>) {
    let s = interp.tape.stats();
    eprintln!(
        "[interp-debug] tape initial_cells={} final_cells={} visited_span={} \
         right_grew_bytes={} ptr_min={} ptr_max={} \
         move_left_units={} move_right_units={}",
        s.initial_len,
        s.final_len,
        s.visited_span(),
        s.right_grew_bytes,
        s.ptr_min,
        s.ptr_max,
        s.move_left_units,
        s.move_right_units,
    );
    if let Some(profile) = interp.profile() {
        eprint!("{}", format_hotspot_report(profile, DEFAULT_HOTSPOT_TOP_N));
    }
}

#[cfg(target_os = "linux")]
fn run_tiered(config: &DriverConfig, hir: &HirProgram) -> Result<()> {
    let threshold = config.jit_threshold.unwrap_or(DEFAULT_JIT_THRESHOLD);
    let mut interp = Interpreter::new(
        DEFAULT_INTERPRETER_TAPE_LEN,
        BufferedStdIo::new(),
        NullHost::new(),
    );
    interp.enable_tiered_jit(threshold);
    if config.interp_debug {
        interp.enable_hotspot_profiling(threshold);
        interp.jit_enabled = true;
    }
    interp.run(hir)?;
    log_info(format!(
        "tiered interpreter finished (hir_insts={} jit_threshold={})",
        hir.insts.len(),
        threshold,
    ));

    if config.interp_debug {
        emit_interp_debug_report(&interp);
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn run_jit(config: &DriverConfig, hir: &HirProgram) -> Result<()> {
    use crate::backend::x86_64::encode::encode_program;

    // O3 special cases still use the H1 exit-based path (no tape needed).
    if config.opt_level == OptLevel::O3 {
        if !hir.has_put_byte() {
            log_info("jit -O3: no output ops; emitting trivial exit(0)");
            let asm = relax_jumps(compile_trivial_exit_asm());
            let encoded = encode_program(&asm);
            let buf = amazingbf_jit::JitBuffer::new(&encoded.text)
                .map_err(|e| crate::error::Error::Jit(e.to_string()))?;
            buf.execute()
                .map_err(|e| crate::error::Error::Jit(e.to_string()))?;
            return Ok(());
        }
        if !hir.has_get_byte() {
            log_info("jit -O3: no input; folding stdout to precomputed write+exit");
            let io = BufferOutputIo::default();
            let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, io, NullHost::new());
            interp.run(hir)?;
            let asm = relax_jumps(compile_precomputed_stdout_asm(&interp.io.bytes));
            let encoded = encode_program(&asm);
            let buf = amazingbf_jit::JitBuffer::new(&encoded.text)
                .map_err(|e| crate::error::Error::Jit(e.to_string()))?;
            buf.execute()
                .map_err(|e| crate::error::Error::Jit(e.to_string()))?;
            return Ok(());
        }
    }

    // H2 ret-based JIT: compile to a callable function, allocate a tape,
    // call the function, and inspect the return code.
    let lir = build_optimized_lir(hir, config.opt_level);
    log_debug(format!("jit: lowered lir (lir_insts={})", lir.len()));
    let asm = relax_jumps(compile_lir_to_jit_asm(&lir));
    let encoded = encode_program(&asm);
    log_debug(format!(
        "jit: encoded x86_64 machine code (text_bytes={})",
        encoded.text.len()
    ));

    let buf = amazingbf_jit::JitBuffer::new(&encoded.text)
        .map_err(|e| crate::error::Error::Jit(e.to_string()))?;

    // Allocate the initial tape via mmap (same size as the AOT backend).
    const JIT_INITIAL_TAPE: usize = 4096;
    let tape = amazingbf_jit::JitTape::new(JIT_INITIAL_TAPE)
        .map_err(|e| crate::error::Error::Jit(e.to_string()))?;

    log_info("jit: executing compiled code (H2 ret-based)");
    let exit_code = buf.execute_fn(tape.base(), tape.data_ptr(), tape.end());

    // The JIT code may have reallocated the tape via its own mmap/munmap,
    // so we must not drop the original JitTape (it may already be freed).
    // Leak it — the OS reclaims all mappings on process exit.
    std::mem::forget(tape);

    if exit_code != 0 {
        return Err(crate::error::Error::Jit(format!(
            "JIT code returned error (exit_code={exit_code})"
        )));
    }

    log_info("jit: execution completed successfully");
    Ok(())
}
