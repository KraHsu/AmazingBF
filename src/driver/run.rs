use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::backend::codegen::compile_lir_to_asm;
use crate::backend::x86_64::debug;
use crate::backend::x86_64::compile_asm_to_elf;
use crate::driver::config::{DriverConfig, RunMode};
use crate::frontend::lexer::lex;
use crate::frontend::parser::parse;
use crate::interp::engine::Interpreter;
use crate::ir::lower::{lower_to_hir, lower_to_lir};
use crate::ir::optimize::optimize;
use crate::runtime::host::NullHost;
use crate::runtime::io::StdIo;
use anyhow::{Context, Result};
use tracing::{debug, info, info_span};

const ASM_LISTING_PATH: &str = "output.asm";
const HEX_LISTING_PATH: &str = "output.lst";

/// from src to HIR
pub fn run(config: DriverConfig) -> Result<()> {
    let run_span = info_span!(
        "driver.run",
        mode = config.mode.as_str(),
        input = %config.input.display(),
        output = %config.output.display(),
        source_bytes = config.source.len()
    );
    let _run_guard = run_span.enter();

    info!("starting pipeline");

    let tokens = lex(&config.source);
    debug!(token_count = tokens.len(), "lexed source");
    let ast = parse(&tokens)?;
    debug!(ast_nodes = ast.len(), "parsed ast");
    let hir = optimize(lower_to_hir(&ast));
    debug!(hir_insts = hir.insts.len(), "lowered and optimized hir");
    let lir = lower_to_lir(&hir);
    debug!(lir_insts = lir.len(), "lowered lir");
    let asm = compile_lir_to_asm(&lir);
    debug!(asm_insts = asm.insts.len(), "generated asm program");

    match config.mode {
        RunMode::Interpret => {
            let io = StdIo::new();
            let host = NullHost::new();
            let mut interp = Interpreter::new(30_000, io, host);

            interp.run(&hir)?;
            info!(hir_insts = hir.insts.len(), "interpreter finished");
        }
        RunMode::Compile => {
            let elf = compile_asm_to_elf(&asm);
            fs::write(&config.output, &elf).with_context(|| {
                format!("failed to write executable to {}", config.output.display())
            })?;
            let mut perms = fs::metadata(&config.output)
                .with_context(|| format!("failed to read metadata for {}", config.output.display()))?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&config.output, perms).with_context(|| {
                format!(
                    "failed to set executable permissions on {}",
                    config.output.display()
                )
            })?;

            write_artifact(ASM_LISTING_PATH, debug::dump_asm_listing(&asm))?;
            write_artifact(HEX_LISTING_PATH, debug::dump_hex_listing(&asm))?;

            info!(
                executable = %config.output.display(),
                elf_bytes = elf.len(),
                asm_insts = asm.insts.len(),
                asm_listing = ASM_LISTING_PATH,
                hex_listing = HEX_LISTING_PATH,
                "compile artifacts written"
            );
        }
        RunMode::Dump => {
            info!(
                hir_insts = hir.insts.len(),
                lir_insts = lir.len(),
                asm_insts = asm.insts.len(),
                "dump mode completed without emitting files"
            );
        }
    }

    info!("pipeline finished");

    Ok(())
}

fn write_artifact(path: impl AsRef<Path>, contents: String) -> Result<()> {
    let path = path.as_ref();
    fs::write(path, contents)
        .with_context(|| format!("failed to write debug artifact {}", path.display()))?;
    info!(artifact = %path.display(), "wrote debug artifact");
    Ok(())
}
