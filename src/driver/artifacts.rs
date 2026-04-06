//! File output helpers for compile-mode artifacts.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::error::io_err;
use crate::logging::log_info;

use crate::driver::config::CompileTarget;

pub(crate) const ASM_LISTING_EXT: &str = "asm";
pub(crate) const HEX_LISTING_EXT: &str = "lst";

pub(crate) fn artifact_path(output: &Path, extension: &str) -> PathBuf {
    output.with_extension(extension)
}

/// Path used for the emitted native executable in compile mode.
///
/// For Windows PE output, if `-o` has no file extension, `.exe` is applied so the default
/// `hello_bf` style path becomes `hello_bf.exe`. An explicit extension (including `.exe`) is kept.
pub(crate) fn compile_executable_output_path(target: CompileTarget, output: &Path) -> PathBuf {
    match target {
        CompileTarget::X86_64Linux => output.to_path_buf(),
        CompileTarget::X86_64Windows => {
            if output.extension().is_none() {
                output.with_extension("exe")
            } else {
                output.to_path_buf()
            }
        }
    }
}

pub(crate) fn write_artifact(path: impl AsRef<Path>, contents: String) -> Result<()> {
    let path = path.as_ref();
    fs::write(path, contents).map_err(|e| io_err(path, "write", e))?;
    log_info(format!("wrote debug artifact {}", path.display()));
    Ok(())
}

pub(crate) fn write_executable(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).map_err(|e| io_err(path, "write executable to", e))?;
    set_executable_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = fs::metadata(path)
        .map_err(|e| io_err(path, "read metadata for", e))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .map_err(|e| io_err(path, "set executable permissions on", e))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::config::CompileTarget;

    #[test]
    fn compile_executable_output_path_windows_adds_exe_when_missing_extension() {
        let p = Path::new("out/hello_bf");
        assert_eq!(
            compile_executable_output_path(CompileTarget::X86_64Windows, p),
            PathBuf::from("out/hello_bf.exe")
        );
    }

    #[test]
    fn compile_executable_output_path_windows_respects_existing_extension() {
        let p = Path::new("out/cross_target.bin");
        assert_eq!(
            compile_executable_output_path(CompileTarget::X86_64Windows, p),
            PathBuf::from("out/cross_target.bin")
        );
    }

    #[test]
    fn compile_executable_output_path_linux_is_unchanged() {
        let p = Path::new("out/hello_bf");
        assert_eq!(
            compile_executable_output_path(CompileTarget::X86_64Linux, p),
            PathBuf::from("out/hello_bf")
        );
    }

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
