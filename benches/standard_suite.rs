//! Criterion benchmarks for the standard matslina Brainfuck suite.
//!
//! Each `BenchCase` pairs a BF program under `benches/bf/` with an optional
//! stdin fixture under `benches/inputs/`. Per case, the harness registers
//! two kinds of measurements:
//!
//! - `interp/O<N>`: time one full run of `AmazingBF -O<N> -m interpret`.
//! - `exec/O<N>`: time one exec of the native-compiled binary (produced
//!   once during setup, out of the measurement loop).
//!
//! The per-program `interpret_levels` / `compile_levels` arrays trim the
//! matrix to combinations that finish in a tractable wall-clock time. `O0`
//! interpret of `hanoi.b`, for example, would run for many minutes, so
//! those cells are excluded.
//!
//! Baseline workflow: `cargo bench --bench standard_suite -- --save-baseline
//! <name>`. Later runs compare against it via `--baseline <name>`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion};

/// Absolute path to the freshly-built `AmazingBF` binary (set by Cargo when
/// this benchmark target is compiled).
const AMAZINGBF_BIN: &str = env!("CARGO_BIN_EXE_AmazingBF");

/// Crate root; used to resolve `benches/` and `target/` paths.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// One program plus its bench matrix configuration.
struct BenchCase {
    /// Short identifier used in Criterion group names.
    name: &'static str,
    /// Source path relative to `benches/`.
    source: &'static str,
    /// Optional stdin fixture path relative to `benches/`.
    stdin: Option<&'static str>,
    /// Optimization levels to register under interpret mode (`"0"..="3"`).
    interpret_levels: &'static [&'static str],
    /// Optimization levels to register under compile-then-exec mode.
    compile_levels: &'static [&'static str],
    /// Criterion sample count; lower for slow benches.
    sample_size: usize,
    /// Total wall time allocated per bench unit, in seconds.
    measurement_time_s: u64,
}

/// Cases shipped with this harness. Edit the per-case level lists to
/// expand or trim the matrix.
const CASES: &[BenchCase] = &[
    BenchCase {
        name: "long",
        source: "bf/long.b",
        stdin: None,
        interpret_levels: &["1", "2", "3"],
        compile_levels: &["0", "1", "2", "3"],
        sample_size: 10,
        measurement_time_s: 30,
    },
    BenchCase {
        name: "dbfi",
        source: "bf/dbfi.b",
        stdin: Some("inputs/dbfi.stdin"),
        interpret_levels: &["0", "1", "2", "3"],
        compile_levels: &["0", "1", "2", "3"],
        sample_size: 15,
        measurement_time_s: 20,
    },
    BenchCase {
        name: "factor",
        source: "bf/factor.b",
        stdin: Some("inputs/factor.stdin"),
        interpret_levels: &["3"],
        compile_levels: &["0", "1", "2", "3"],
        sample_size: 10,
        measurement_time_s: 30,
    },
    BenchCase {
        name: "mandelbrot",
        source: "bf/mandelbrot.b",
        stdin: None,
        interpret_levels: &["3"],
        compile_levels: &["1", "2", "3"],
        sample_size: 10,
        measurement_time_s: 60,
    },
    BenchCase {
        name: "hanoi",
        source: "bf/hanoi.b",
        stdin: None,
        interpret_levels: &[],
        compile_levels: &["2", "3"],
        sample_size: 10,
        measurement_time_s: 60,
    },
];

fn bench_root() -> PathBuf {
    PathBuf::from(MANIFEST_DIR).join("benches")
}

fn source_path(case: &BenchCase) -> PathBuf {
    bench_root().join(case.source)
}

fn stdin_path(case: &BenchCase) -> Option<PathBuf> {
    case.stdin.map(|p| bench_root().join(p))
}

fn out_dir() -> PathBuf {
    PathBuf::from(MANIFEST_DIR).join("target").join("bench-bin")
}

fn load_stdin(case: &BenchCase) -> Option<Vec<u8>> {
    stdin_path(case)
        .map(|p| std::fs::read(&p).unwrap_or_else(|e| panic!("read stdin fixture {:?}: {}", p, e)))
}

/// Drive the child process, feeding `stdin_bytes` through a pipe when
/// present, and wait for exit. Used by both interpret and compiled-exec
/// measurement paths.
fn spawn_and_feed(mut cmd: Command, stdin_bytes: Option<&[u8]>) -> std::process::ExitStatus {
    if stdin_bytes.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = cmd.spawn().expect("spawn child process");
    if let (Some(bytes), Some(mut stdin)) = (stdin_bytes, child.stdin.take()) {
        stdin.write_all(bytes).expect("write stdin to child");
        drop(stdin);
    }
    child.wait().expect("wait child")
}

/// Compile `case` at the given optimization level and return the path to
/// the produced binary. Runs in setup, outside the measurement loop.
fn compile_once(case: &BenchCase, level: &str) -> PathBuf {
    let out_root = out_dir();
    std::fs::create_dir_all(&out_root).expect("create bench-bin dir");
    let elf = out_root.join(format!("{}_O{}", case.name, level));

    let status = Command::new(AMAZINGBF_BIN)
        .arg(source_path(case))
        .arg("-m")
        .arg("compile")
        .arg(format!("-O{}", level))
        .arg("-o")
        .arg(&elf)
        .arg("-q")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .expect("spawn AmazingBF for compile step");
    assert!(
        status.success(),
        "compile of {} at O{} failed: {:?}",
        case.name,
        level,
        status
    );
    elf
}

fn run_interp(case: &BenchCase, level: &str, stdin_bytes: Option<&[u8]>) {
    let mut cmd = Command::new(AMAZINGBF_BIN);
    cmd.arg(source_path(case))
        .arg(format!("-O{}", level))
        .arg("-m")
        .arg("interpret")
        .arg("-q")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = spawn_and_feed(cmd, stdin_bytes);
    assert!(
        status.success(),
        "interpret of {} O{} failed: {:?}",
        case.name,
        level,
        status
    );
}

fn run_compiled(elf: &Path, case: &BenchCase, stdin_bytes: Option<&[u8]>) {
    let mut cmd = Command::new(elf);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let status = spawn_and_feed(cmd, stdin_bytes);
    assert!(
        status.success(),
        "compiled {} exited with failure: {:?}",
        case.name,
        status
    );
}

fn bench_all(c: &mut Criterion) {
    for case in CASES {
        let mut group = c.benchmark_group(case.name);
        group.sample_size(case.sample_size);
        group.measurement_time(Duration::from_secs(case.measurement_time_s));
        let warmup = Duration::from_millis((case.measurement_time_s * 100).max(1000));
        group.warm_up_time(warmup);

        let stdin_bytes = load_stdin(case);

        for level in case.interpret_levels {
            group.bench_with_input(
                BenchmarkId::new("interp", format!("O{}", level)),
                level,
                |b, lvl| {
                    b.iter(|| run_interp(case, lvl, stdin_bytes.as_deref()));
                },
            );
        }

        for level in case.compile_levels {
            let elf = compile_once(case, level);
            group.bench_with_input(
                BenchmarkId::new("exec", format!("O{}", level)),
                level,
                |b, _lvl| {
                    b.iter(|| run_compiled(&elf, case, stdin_bytes.as_deref()));
                },
            );
        }

        group.finish();
    }
}

fn main() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_all(&mut criterion);
    criterion.final_summary();
}
