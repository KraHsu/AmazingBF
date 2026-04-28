//! Command-line argument parsing for all four shipped binaries.
//!
//! `parse_cli` / `parse_interpreter_cli` / `parse_compiler_cli` share a single
//! state machine parameterised by `Flavor`: the `AmazingBF` entry point
//! accepts every flag, while the specialised `bf-interpreter` / `bf-compiler`
//! entries disallow mode-switching. User-facing help and error text is
//! bilingual (Chinese + English) as mandated by CLAUDE.md; the resulting
//! `AppConfig` carries a fully-validated `DriverConfig` plus log level.

use crate::driver::config::{CompileTarget, DriverConfig, OptLevel, RunMode};

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const LONG_ABOUT: &str = "\
AmazingBF 将 Brainfuck 源码走完整前端与中间层：词法/语法 → AST → HIR（可优化）→ LIR → \
（解释执行或 x86_64 原生后端）。\
默认在 HIR 上解释运行；compile 模式可通过 --target 选择 x86_64-linux ELF 或 x86_64-windows PE64，并在输出路径旁写入汇编与十六进制 listing。";

const AFTER_LONG_HELP: &str = "\
示例:
  # 解释执行（默认）
  AmazingBF path/to/hello.bf

  # 从标准输入读入源码
  cat hello.bf | AmazingBF -

  # 编译为可执行文件并运行
  AmazingBF path/to/hello.bf -m compile -o ./hello_bf
  ./hello_bf

  # 显式指定编译目标
  AmazingBF path/to/hello.bf -m compile --target x86_64-linux -o ./hello_bf

  # 只跑通流水线并在日志里看各阶段规模（不写文件）
  AmazingBF path/to/hello.bf -m dump -vv

  # 解释执行并在 stderr 打印 tape 使用统计
  AmazingBF path/to/hello.bf --interp-debug

  # 特化入口（无需 -m）：bf-interpreter / bf-compiler
  bf-interpreter path/to/hello.bf
  bf-compiler path/to/hello.bf -o ./hello_bf

手册页（若已安装）: man amazingbf";

const INTERP_LONG_ABOUT: &str = "\
在优化后的 HIR 上解释执行 Brainfuck。与 `AmazingBF -m interpret` 等价，无需 `-m`。";

const INTERP_AFTER_HELP: &str = "\
示例:
  bf-interpreter path/to/hello.bf
  cat hello.bf | bf-interpreter -";

const COMPILER_LONG_ABOUT: &str = "\
将 Brainfuck 编译为 x86_64 原生可执行文件，并在 `-o` 指定路径旁生成 `.asm` / `.lst`。\
与 `AmazingBF -m compile` 等价；默认目标平台跟随构建本二进制时的 target（Linux 构建产物默认生成 ELF，Windows 构建产物默认生成 PE64），也可通过 `--target` 选择交叉编译。";

const COMPILER_AFTER_HELP: &str = "\
示例:
  bf-compiler path/to/hello.bf -o ./hello_bf
  ./hello_bf

  # 交叉编译为 Windows PE64
  bf-compiler path/to/hello.bf --target x86_64-windows -o ./hello_bf.exe";

/// Identifies which binary entry point is parsing arguments.
#[derive(Clone, Copy, Debug)]
pub(crate) enum BinKind {
    /// The full `AmazingBF` driver: accepts every flag including `-m`.
    AmazingBF,
    /// `bf-interpreter`: forces `RunMode::Interpret`; rejects `-m` / `--target`.
    Interpreter,
    /// `bf-compiler`: forces `RunMode::Compile`; rejects `--interp-debug`.
    Compiler,
}

impl BinKind {
    /// Human-readable program name used in help / version banners.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::AmazingBF => "AmazingBF",
            Self::Interpreter => "bf-interpreter",
            Self::Compiler => "bf-compiler",
        }
    }
}

/// Fully-validated configuration handed from `cli` to `driver::run`.
#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    /// Normalised driver configuration for the selected pipeline stage.
    pub(crate) driver_cfg: DriverConfig,

    /// 0 -> quiet
    /// 1 -> normal
    /// 2 -> v
    /// 3 -> vv
    /// _ -> vvv
    pub(crate) log_level: u8,
}

/// Outcome of CLI parsing — either an early exit (help/version) or a failure.
#[derive(Debug)]
pub(crate) enum CliError {
    /// User requested `-h` / `--help`; caller should print help for this binary and exit 0.
    Help(BinKind),
    /// User requested `-V` / `--version`; caller should print the version banner and exit 0.
    Version(BinKind),
    /// Invalid flag combination or missing argument; caller should print `message` and exit 2.
    Usage {
        /// Bilingual error text intended for stderr.
        message: String,
        /// When true, the user passed `-q`; suppress additional hints.
        quiet: bool,
    },
    /// Generic parse-time diagnostic with a custom exit code.
    Message {
        /// Bilingual error text intended for stderr.
        message: String,
        /// When true, suppress auxiliary hints.
        quiet: bool,
        /// Process exit code to return after printing `message`.
        exit_code: i32,
    },
    /// Failed to load the input source file (e.g. missing path, permission denied).
    Io {
        /// Path that failed to load, or `"-"` for stdin.
        path: String,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Help(_) => f.write_str("help"),
            CliError::Version(_) => f.write_str("version"),
            CliError::Usage { message, .. } | CliError::Message { message, .. } => {
                f.write_str(message)
            }
            CliError::Io { path, source } => {
                write!(f, "错误: 无法读取源 `{path}`: {source}")
            }
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Flavor {
    Full,
    Interpreter,
    Compiler,
}

#[derive(Debug)]
struct PartialCore {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    verbose: u8,
    quiet: bool,
    opt_level: OptLevel,
    mode: RunMode,
    target: CompileTarget,
    interp_debug: bool,
    #[cfg(target_os = "linux")]
    jit_threshold: Option<u64>,
}

fn is_positional(arg: &OsStr) -> bool {
    if arg == "-" {
        return true;
    }
    let Some(s) = arg.to_str() else {
        return true;
    };
    !s.starts_with('-')
}

fn load_source(input: &PathBuf) -> std::result::Result<String, CliError> {
    if input.as_os_str() == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|source| CliError::Io {
                path: "-".to_string(),
                source,
            })?;
        Ok(buf)
    } else {
        std::fs::read_to_string(input).map_err(|source| CliError::Io {
            path: input.display().to_string(),
            source,
        })
    }
}

fn quiet_requested() -> bool {
    std::env::args_os().any(|arg| arg == "-q" || arg == "--quiet")
}

fn validate_log_level(verbose: u8, quiet: bool) -> std::result::Result<(), CliError> {
    if verbose > 3 {
        return Err(CliError::Message {
            message: "错误: 详细级别最多为 3（即 -vvv）".to_string(),
            quiet,
            exit_code: 1,
        });
    }
    Ok(())
}

fn finish_from_partial(
    core: PartialCore,
    flavor: Flavor,
) -> std::result::Result<AppConfig, CliError> {
    let (mode, target, interp_debug) = match flavor {
        Flavor::Full => (core.mode, core.target, core.interp_debug),
        Flavor::Interpreter => (
            RunMode::Interpret,
            CompileTarget::build_default(),
            core.interp_debug,
        ),
        Flavor::Compiler => (RunMode::Compile, core.target, false),
    };

    validate_log_level(core.verbose, core.quiet)?;
    let input = core.input.ok_or_else(|| CliError::Usage {
        message: "错误: 缺少输入文件（或使用 `-` 表示标准输入）".to_string(),
        quiet: core.quiet,
    })?;
    let source = load_source(&input)?;

    Ok(AppConfig {
        driver_cfg: DriverConfig {
            source,
            mode,
            target,
            output: core
                .output
                .unwrap_or_else(|| PathBuf::from(target.default_output_name())),
            interp_debug,
            opt_level: core.opt_level,
            #[cfg(target_os = "linux")]
            jit_threshold: core.jit_threshold,
        },
        log_level: if core.quiet { 0 } else { core.verbose + 1 },
    })
}

fn take_value<'a>(
    i: &mut usize,
    args: &'a [OsString],
    flag: &str,
) -> std::result::Result<&'a OsStr, CliError> {
    *i += 1;
    let v = args.get(*i).ok_or_else(|| CliError::Usage {
        message: format!("错误: `{flag}` 需要参数"),
        quiet: quiet_requested(),
    })?;
    Ok(v.as_os_str())
}

fn parse_opt_level_value(raw: &str) -> std::result::Result<OptLevel, CliError> {
    OptLevel::parse(raw).ok_or_else(|| CliError::Usage {
        message: format!("错误: 无效的优化级别 `{raw}`（应为 0、1、2 或 3）"),
        quiet: quiet_requested(),
    })
}

fn parse_short_cluster(
    cluster: &str,
    flavor: Flavor,
    i: &mut usize,
    args: &[OsString],
    core: &mut PartialCore,
    bin: BinKind,
) -> std::result::Result<(), CliError> {
    let mut chars = cluster.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'h' => return Err(CliError::Help(bin)),
            'V' => return Err(CliError::Version(bin)),
            'v' => core.verbose = core.verbose.saturating_add(1),
            'q' => core.quiet = true,
            'm' => {
                if !matches!(flavor, Flavor::Full) {
                    return Err(CliError::Usage {
                        message: "错误: 此入口不支持 `-m`".to_string(),
                        quiet: quiet_requested(),
                    });
                }
                let tail: String = chars.collect();
                let raw = if tail.is_empty() {
                    take_value(i, args, "-m")?.to_string_lossy().into_owned()
                } else {
                    tail
                };
                let Some(m) = RunMode::parse(&raw) else {
                    return Err(CliError::Usage {
                        message: format!("错误: 无效的运行模式 `{raw}`"),
                        quiet: quiet_requested(),
                    });
                };
                core.mode = m;
                return Ok(());
            }
            'o' => {
                let tail: String = chars.collect();
                if tail.is_empty() {
                    let p = take_value(i, args, "-o")?;
                    core.output = Some(PathBuf::from(p));
                } else {
                    core.output = Some(PathBuf::from(tail));
                }
                return Ok(());
            }
            'O' => {
                let tail: String = chars.collect();
                let raw = if tail.is_empty() {
                    take_value(i, args, "-O")?.to_string_lossy().into_owned()
                } else {
                    tail
                };
                core.opt_level = parse_opt_level_value(&raw)?;
                return Ok(());
            }
            _ => {
                return Err(CliError::Usage {
                    message: format!("错误: 未知短选项 `-{c}`"),
                    quiet: quiet_requested(),
                });
            }
        }
    }
    Ok(())
}

fn parse_long(
    name: &str,
    value: Option<&str>,
    flavor: Flavor,
    i: &mut usize,
    args: &[OsString],
    core: &mut PartialCore,
    bin: BinKind,
) -> std::result::Result<(), CliError> {
    let quiet = quiet_requested();
    match name {
        "help" => Err(CliError::Help(bin)),
        "version" => Err(CliError::Version(bin)),
        "verbose" => {
            core.verbose = core.verbose.saturating_add(1);
            Ok(())
        }
        "quiet" => {
            core.quiet = true;
            Ok(())
        }
        "output" => {
            let raw = if let Some(v) = value {
                v
            } else {
                take_value(i, args, "--output")?
                    .to_str()
                    .ok_or_else(|| CliError::Usage {
                        message: "错误: `--output` 路径须为有效 UTF-8".to_string(),
                        quiet,
                    })?
            };
            core.output = Some(PathBuf::from(raw));
            Ok(())
        }
        "opt-level" => {
            let raw = if let Some(v) = value {
                v
            } else {
                take_value(i, args, "--opt-level")?
                    .to_str()
                    .ok_or_else(|| CliError::Usage {
                        message: "错误: `--opt-level` 参数须为有效 UTF-8".to_string(),
                        quiet,
                    })?
            };
            core.opt_level = parse_opt_level_value(raw)?;
            Ok(())
        }
        "mode" if matches!(flavor, Flavor::Full) => {
            let raw = if let Some(v) = value {
                v
            } else {
                take_value(i, args, "--mode")?
                    .to_str()
                    .ok_or_else(|| CliError::Usage {
                        message: "错误: `--mode` 参数须为有效 UTF-8".to_string(),
                        quiet,
                    })?
            };
            let Some(m) = RunMode::parse(raw) else {
                return Err(CliError::Usage {
                    message: format!("错误: 无效的运行模式 `{raw}`"),
                    quiet,
                });
            };
            core.mode = m;
            Ok(())
        }
        "target" if matches!(flavor, Flavor::Full | Flavor::Compiler) => {
            let raw = if let Some(v) = value {
                v
            } else {
                take_value(i, args, "--target")?
                    .to_str()
                    .ok_or_else(|| CliError::Usage {
                        message: "错误: `--target` 参数须为有效 UTF-8".to_string(),
                        quiet,
                    })?
            };
            let Some(t) = CompileTarget::parse(raw) else {
                return Err(CliError::Usage {
                    message: format!("错误: 无效的编译目标 `{raw}`"),
                    quiet,
                });
            };
            core.target = t;
            Ok(())
        }
        "interp-debug" if matches!(flavor, Flavor::Full | Flavor::Interpreter) => {
            core.interp_debug = value != Some("false");
            Ok(())
        }
        #[cfg(target_os = "linux")]
        "jit-threshold" if matches!(flavor, Flavor::Full) => {
            let raw = if let Some(v) = value {
                v
            } else {
                take_value(i, args, "--jit-threshold")?
                    .to_str()
                    .ok_or_else(|| CliError::Usage {
                        message: "错误: `--jit-threshold` 参数须为有效 UTF-8".to_string(),
                        quiet,
                    })?
            };
            let n = raw.parse::<u64>().map_err(|_| CliError::Usage {
                message: format!("错误: 无效的 `--jit-threshold` 值 `{raw}`（须为非负整数）"),
                quiet,
            })?;
            core.jit_threshold = Some(n);
            Ok(())
        }
        _ => Err(CliError::Usage {
            message: format!("错误: 未知选项 `--{name}`"),
            quiet,
        }),
    }
}

fn parse_args(flavor: Flavor) -> std::result::Result<AppConfig, CliError> {
    let bin = match flavor {
        Flavor::Full => BinKind::AmazingBF,
        Flavor::Interpreter => BinKind::Interpreter,
        Flavor::Compiler => BinKind::Compiler,
    };

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut core = PartialCore {
        input: None,
        output: None,
        verbose: 0,
        quiet: false,
        opt_level: OptLevel::O3,
        mode: RunMode::Interpret,
        target: CompileTarget::build_default(),
        interp_debug: false,
        #[cfg(target_os = "linux")]
        jit_threshold: None,
    };

    let mut i = 0usize;
    let mut after_dd = false;

    while i < args.len() {
        let arg_os = &args[i];
        if after_dd || is_positional(arg_os.as_os_str()) {
            let path = PathBuf::from(arg_os);
            if core.input.is_some() {
                return Err(CliError::Usage {
                    message: "错误: 多余的输入路径（只接受一个 FILE）".to_string(),
                    quiet: quiet_requested(),
                });
            }
            core.input = Some(path);
            i += 1;
            continue;
        }

        let Some(s) = arg_os.to_str() else {
            return Err(CliError::Usage {
                message: "错误: 无法解析的命令行参数（非 UTF-8）".to_string(),
                quiet: quiet_requested(),
            });
        };

        if s == "--" {
            after_dd = true;
            i += 1;
            continue;
        }

        if let Some(rest) = s.strip_prefix("--") {
            if rest.is_empty() {
                return Err(CliError::Usage {
                    message: "错误: 无效的 `--`".to_string(),
                    quiet: quiet_requested(),
                });
            }
            let (name, eq_val) = rest
                .split_once('=')
                .map(|(a, b)| (a, Some(b)))
                .unwrap_or((rest, None));

            match (name, flavor) {
                ("mode", Flavor::Interpreter | Flavor::Compiler) => {
                    return Err(CliError::Usage {
                        message: "错误: 此入口不支持 `--mode`".to_string(),
                        quiet: quiet_requested(),
                    });
                }
                ("target", Flavor::Interpreter) => {
                    return Err(CliError::Usage {
                        message: "错误: `bf-interpreter` 不支持 `--target`".to_string(),
                        quiet: quiet_requested(),
                    });
                }
                ("interp-debug", Flavor::Compiler) => {
                    return Err(CliError::Usage {
                        message: "错误: `bf-compiler` 不支持 `--interp-debug`".to_string(),
                        quiet: quiet_requested(),
                    });
                }
                _ => {}
            }

            parse_long(name, eq_val, flavor, &mut i, &args, &mut core, bin)?;
            i += 1;
            continue;
        }

        // Short flags
        let body = &s[1..];
        if body.is_empty() {
            return Err(CliError::Usage {
                message: "错误: 单独的 `-` 请用作输入文件（stdin）".to_string(),
                quiet: quiet_requested(),
            });
        }

        parse_short_cluster(body, flavor, &mut i, &args, &mut core, bin)?;
        i += 1;
    }

    if core.quiet && core.verbose > 0 {
        return Err(CliError::Usage {
            message: "错误: 不能同时使用 `-q` 与 `-v`".to_string(),
            quiet: true,
        });
    }

    if matches!(flavor, Flavor::Interpreter) && core.verbose == 0 {
        core.quiet = true;
    }

    finish_from_partial(core, flavor)
}

/// Print bilingual help text for the given binary flavor to stderr.
pub(crate) fn print_help(kind: BinKind) {
    let title = kind.title();
    match kind {
        BinKind::AmazingBF => {
            eprintln!(
                "{title} {VERSION}\nBrainfuck 编译器与解释器（HIR / x86_64 native backend）\n\n{LONG_ABOUT}\n"
            );
            eprintln!(
                "用法:\n  {title} [选项] <FILE>\n\n\
                 参数:\n  <FILE>  Brainfuck 源文件路径；`-` 表示从标准输入读取\n\n\
                 选项:\n  -h, --help              显示帮助\n  -V, --version           显示版本\n  -v, --verbose           提高日志详细度（可重复，最多 -vvv）\n  -q, --quiet             静默日志\n  -o, --output <PATH>     编译输出路径（compile 模式）\n  -O, --opt-level <0-3>   优化级别（默认 3）\n  -m, --mode <MODE>       运行模式: dump | interpret | compile | jit | tiered（默认 interpret）\n      --target <T>        编译目标: x86_64-linux | x86_64-windows\n      --interp-debug      解释模式下输出 tape 统计\n      --jit-threshold <N> tiered 模式下热点循环触发 JIT 的迭代次数阈值（默认 10000，仅 Linux）\n"
            );
            eprintln!("{AFTER_LONG_HELP}");
        }
        BinKind::Interpreter => {
            eprintln!("{title} {VERSION}\nBrainfuck HIR 解释器\n\n{INTERP_LONG_ABOUT}\n");
            eprintln!(
                "用法:\n  {title} [选项] <FILE>\n\n\
                 参数:\n  <FILE>  Brainfuck 源文件路径；`-` 表示从标准输入读取\n\n\
                 选项:\n  -h, --help              显示帮助\n  -V, --version           显示版本\n  -v, --verbose           提高日志详细度（默认同 `-q`）\n  -q, --quiet             静默日志\n  -o, --output <PATH>     （解释模式下忽略）\n  -O, --opt-level <0-3>   优化级别（默认 3）\n      --interp-debug      运行结束后输出 tape 统计\n"
            );
            eprintln!("{INTERP_AFTER_HELP}");
        }
        BinKind::Compiler => {
            eprintln!(
                "{title} {VERSION}\nBrainfuck → x86_64 原生编译器\n\n{COMPILER_LONG_ABOUT}\n"
            );
            eprintln!(
                "用法:\n  {title} [选项] <FILE>\n\n\
                 参数:\n  <FILE>  Brainfuck 源文件路径；`-` 表示从标准输入读取\n\n\
                 选项:\n  -h, --help              显示帮助\n  -V, --version           显示版本\n  -v, --verbose           提高日志详细度\n  -q, --quiet             静默日志\n  -o, --output <PATH>     编译输出路径\n  -O, --opt-level <0-3>   优化级别（默认 3）\n      --target <T>        x86_64-linux | x86_64-windows\n"
            );
            eprintln!("{COMPILER_AFTER_HELP}");
        }
    }
}

/// Parse argv for the full `AmazingBF` binary (all modes available).
pub(crate) fn parse_cli() -> std::result::Result<AppConfig, CliError> {
    parse_args(Flavor::Full)
}

/// Parse argv for the `bf-interpreter` binary (locked to `RunMode::Interpret`).
pub(crate) fn parse_interpreter_cli() -> std::result::Result<AppConfig, CliError> {
    parse_args(Flavor::Interpreter)
}

/// Parse argv for the `bf-compiler` binary (locked to `RunMode::Compile`).
pub(crate) fn parse_compiler_cli() -> std::result::Result<AppConfig, CliError> {
    parse_args(Flavor::Compiler)
}
