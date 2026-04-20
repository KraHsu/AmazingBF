//! Mode dispatch: interpret / compile / dump, after the frontend has produced HIR.
//!
//! `run()` accepts a fully-validated `DriverConfig`, invokes the shared frontend,
//! then routes HIR into the interpreter, into LIR→asm→ELF/PE64 codegen, or
//! simply logs pipeline sizes without writing any artifact.

use crate::Result;
use crate::backend::asm::AsmProgram;
use crate::backend::codegen::{
    compile_lir_to_asm, compile_precomputed_stdout_asm, compile_trivial_exit_asm,
};
use crate::backend::x86_64::debug;
use crate::backend::x86_64::windows::{
    WindowsProgram, compile_lir_to_windows_program, compile_precomputed_stdout_program,
    compile_trivial_exit_program,
};
use crate::backend::x86_64::{compile_asm_to_elf, compile_windows_program_to_pe};
use crate::driver::artifacts::{
    ASM_LISTING_EXT, HEX_LISTING_EXT, artifact_path, compile_executable_output_path,
    write_artifact, write_executable,
};
use crate::driver::config::{
    CompileTarget, DEFAULT_INTERPRETER_TAPE_LEN, DriverConfig, OptLevel, RunMode,
};
use crate::driver::pipeline::build_frontend;
use crate::interp::engine::Interpreter;
use crate::ir::hir::HirProgram;
use crate::ir::lir_opt::optimize_lir;
use crate::ir::lower::lower_to_lir;
use crate::logging::{log_debug, log_info};
use crate::runtime::host::NullHost;
use crate::runtime::io::{BufferOutputIo, StdIo};

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
        RunMode::Dump => run_dump(&frontend.hir),
    }

    log_info("pipeline finished");

    Ok(())
}

fn run_interpret(config: &DriverConfig, hir: &HirProgram) -> Result<()> {
    let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, StdIo::new(), NullHost::new());
    interp.run(hir)?;
    log_info(format!(
        "interpreter finished (hir_insts={})",
        hir.insts.len()
    ));

    if config.interp_debug {
        let s = interp.tape.stats();
        eprintln!(
            "[interp-debug] tape initial_cells={} final_cells={} visited_span={} \
             right_growth_cells={} ptr_min={} ptr_max={} \
             move_left_units={} move_right_units={}",
            s.initial_len,
            s.final_len,
            s.visited_span(),
            s.right_growth,
            s.ptr_min,
            s.ptr_max,
            s.move_left_units,
            s.move_right_units,
        );
    }

    Ok(())
}

fn run_compile(config: &DriverConfig, hir: &HirProgram) -> Result<()> {
    let output = compile_executable_output_path(config.target, &config.output);
    let asm_listing_path = artifact_path(&output, ASM_LISTING_EXT);
    let hex_listing_path = artifact_path(&output, HEX_LISTING_EXT);
    let (asm, executable) = match config.target {
        CompileTarget::X86_64Linux => {
            let asm = compile_linux_asm(hir, config.opt_level)?;
            let executable = compile_asm_to_elf(&asm);
            (asm, executable)
        }
        CompileTarget::X86_64Windows => {
            let program = compile_windows_program(hir, config.opt_level)?;
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

fn run_dump(hir: &HirProgram) {
    let lir = optimize_lir(lower_to_lir(hir));
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

    let lir = optimize_lir(lower_to_lir(hir));
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

    let lir = optimize_lir(lower_to_lir(hir));
    log_debug(format!("lowered lir (lir_insts={})", lir.len()));
    Ok(compile_lir_to_windows_program(&lir))
}
