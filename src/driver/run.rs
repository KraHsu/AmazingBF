use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::backend::codegen::compile_lir_to_asm;
use crate::backend::x86_64::compile_asm_to_elf;
use crate::backend::x86_64::debug;
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

const ASM_LISTING_EXT: &str = "asm";
const HEX_LISTING_EXT: &str = "lst";

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

    match config.mode {
        RunMode::Interpret => {
            let io = StdIo::new();
            let host = NullHost::new();
            let mut interp = Interpreter::new(30_000, io, host);

            interp.run(&hir)?;
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
        }
        RunMode::Compile => {
            let lir = lower_to_lir(&hir);
            debug!(lir_insts = lir.len(), "lowered lir");
            let asm = compile_lir_to_asm(&lir);
            debug!(asm_insts = asm.insts.len(), "generated asm program");

            let elf = compile_asm_to_elf(&asm);
            let asm_listing_path = artifact_path(&config.output, ASM_LISTING_EXT);
            let hex_listing_path = artifact_path(&config.output, HEX_LISTING_EXT);
            fs::write(&config.output, &elf).with_context(|| {
                format!("failed to write executable to {}", config.output.display())
            })?;
            let mut perms = fs::metadata(&config.output)
                .with_context(|| {
                    format!("failed to read metadata for {}", config.output.display())
                })?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&config.output, perms).with_context(|| {
                format!(
                    "failed to set executable permissions on {}",
                    config.output.display()
                )
            })?;

            write_artifact(&asm_listing_path, debug::dump_asm_listing(&asm))?;
            write_artifact(&hex_listing_path, debug::dump_hex_listing(&asm))?;

            info!(
                executable = %config.output.display(),
                elf_bytes = elf.len(),
                asm_insts = asm.insts.len(),
                asm_listing = %asm_listing_path.display(),
                hex_listing = %hex_listing_path.display(),
                "compile artifacts written"
            );
        }
        RunMode::Dump => {
            let lir = lower_to_lir(&hir);
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

fn artifact_path(output: &Path, extension: &str) -> PathBuf {
    output.with_extension(extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_path_reuses_output_basename() {
        let output = Path::new("build/hello_bf");

        assert_eq!(
            artifact_path(output, "asm"),
            PathBuf::from("build/hello_bf.asm")
        );
        assert_eq!(
            artifact_path(output, "lst"),
            PathBuf::from("build/hello_bf.lst")
        );
    }

    #[test]
    fn artifact_path_replaces_existing_extension() {
        let output = Path::new("build/hello_bf.bin");

        assert_eq!(
            artifact_path(output, "asm"),
            PathBuf::from("build/hello_bf.asm")
        );
    }

    #[test]
    fn artifact_path_for_default_a_out_is_a_asm() {
        let output = Path::new("build/a.out");

        assert_eq!(artifact_path(output, "asm"), PathBuf::from("build/a.asm"));
        assert_eq!(artifact_path(output, "lst"), PathBuf::from("build/a.lst"));
    }
}
