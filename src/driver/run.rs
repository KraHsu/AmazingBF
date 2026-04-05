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
use crate::ir::lower::lower_to_lir;
use crate::runtime::host::NullHost;
use crate::runtime::io::{BufferOutputIo, StdIo};
use anyhow::Result;
use tracing::{debug, info, info_span};

/// Execute the configured pipeline after CLI / logging setup has completed.
pub(crate) fn run(config: DriverConfig) -> Result<()> {
    let run_span = info_span!(
        "driver.run",
        mode = config.mode.as_str(),
        target = config.target.as_str(),
        input = %config.input.display(),
        output = %config.output.display(),
        source_bytes = config.source.len()
    );
    let _run_guard = run_span.enter();

    info!("starting pipeline");

    let frontend = build_frontend(&config)?;
    debug!(
        token_count = frontend.token_count,
        ast_nodes = frontend.ast_nodes,
        hir_insts = frontend.hir.insts.len(),
        "frontend pipeline completed"
    );

    match config.mode {
        RunMode::Interpret => run_interpret(&config, &frontend.hir)?,
        RunMode::Compile => run_compile(&config, &frontend.hir)?,
        RunMode::Dump => run_dump(&frontend.hir),
    }

    info!("pipeline finished");

    Ok(())
}

fn run_interpret(config: &DriverConfig, hir: &HirProgram) -> Result<()> {
    let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, StdIo::new(), NullHost::new());
    interp.run(hir)?;
    info!(hir_insts = hir.insts.len(), "interpreter finished");

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
    debug!(asm_insts = asm.insts.len(), "generated asm program");
    write_executable(&output, &executable)?;
    write_artifact(&asm_listing_path, debug::dump_asm_listing(&asm))?;
    write_artifact(&hex_listing_path, debug::dump_hex_listing(&asm))?;

    info!(
        executable_target = config.target.as_str(),
        executable = %output.display(),
        executable_bytes = executable.len(),
        asm_insts = asm.insts.len(),
        asm_listing = %asm_listing_path.display(),
        hex_listing = %hex_listing_path.display(),
        "compile artifacts written"
    );

    Ok(())
}

fn run_dump(hir: &HirProgram) {
    let lir = lower_to_lir(hir);
    debug!(lir_insts = lir.len(), "lowered lir");
    let asm = compile_lir_to_asm(&lir);
    debug!(asm_insts = asm.insts.len(), "generated asm program");

    info!(
        hir_insts = hir.insts.len(),
        lir_insts = lir.len(),
        asm_insts = asm.insts.len(),
        "dump mode completed without emitting files"
    );
}

fn compile_linux_asm(hir: &HirProgram, opt_level: OptLevel) -> Result<AsmProgram> {
    if opt_level == OptLevel::O3 {
        if !hir.has_put_byte() {
            info!("compile -O3: no output ops; emitting trivial exit(0) ELF");
            return Ok(compile_trivial_exit_asm());
        }
        if !hir.has_get_byte() {
            info!("compile -O3: no input; folding stdout to precomputed write+exit");
            let io = BufferOutputIo::default();
            let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, io, NullHost::new());
            interp.run(hir)?;
            return Ok(compile_precomputed_stdout_asm(&interp.io.bytes));
        }
    }

    let lir = lower_to_lir(hir);
    debug!(lir_insts = lir.len(), "lowered lir");
    Ok(compile_lir_to_asm(&lir))
}

fn compile_windows_program(hir: &HirProgram, opt_level: OptLevel) -> Result<WindowsProgram> {
    if opt_level == OptLevel::O3 {
        if !hir.has_put_byte() {
            info!("compile -O3: no output ops; emitting trivial exit(0) PE");
            return Ok(compile_trivial_exit_program(0));
        }
        if !hir.has_get_byte() {
            info!("compile -O3: no input; folding stdout to precomputed WriteFile+ExitProcess");
            let io = BufferOutputIo::default();
            let mut interp = Interpreter::new(DEFAULT_INTERPRETER_TAPE_LEN, io, NullHost::new());
            interp.run(hir)?;
            return Ok(compile_precomputed_stdout_program(&interp.io.bytes));
        }
    }

    let lir = lower_to_lir(hir);
    debug!(lir_insts = lir.len(), "lowered lir");
    Ok(compile_lir_to_windows_program(&lir))
}
