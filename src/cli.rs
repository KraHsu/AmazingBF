use crate::driver::config::{CompileTarget, DriverConfig, OptLevel, RunMode};

use anyhow::Result;
use clap::{ArgAction, ColorChoice, Parser, error::ErrorKind};
use std::io::Read;
use std::path::PathBuf;

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

/// Shared flags for every frontend (input, logging, optimization, output path).
#[derive(Parser, Debug)]
struct CoreCli {
    /// Brainfuck 源文件路径；使用 `-` 表示从标准输入读取
    #[arg(value_name = "FILE")]
    input: PathBuf,

    /// 编译输出路径（`compile` / `bf-compiler`）；会生成同基名的 .asm / .lst；解释模式下忽略。默认随 target：Linux 为 `a.out`，Windows 为 `a.exe`
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// 提高日志详细度（可重复：-v / -vv / -vvv）
    #[arg(short, long, action = ArgAction::Count, group = "log_level")]
    verbose: u8,

    /// 静默日志（与 -v 互斥）
    #[arg(short, long, action = ArgAction::SetTrue, group = "log_level")]
    quiet: bool,

    /// 编译优化级别：`-O0` 仅合并连续位移/加减；`-O1` 单次窥孔与 `[-]` 等循环化简；`-O2` 重复窥孔直至不动点；`-O3` 在 compile 模式下还启用整程序折叠（HIR 同 `-O2`）
    #[arg(short = 'O', long = "opt-level", value_enum, default_value_t = OptLevel::O0)]
    opt_level: OptLevel,
}

#[derive(Parser, Debug)]
struct InterpFlags {
    /// 解释执行结束后在 stderr 输出 tape 统计（指针范围、向右扩容、左右移动量等）
    #[arg(long = "interp-debug")]
    interp_debug: bool,
}

#[derive(Parser, Debug)]
#[command(
    name = "AmazingBF",
    version,
    about = "Brainfuck 编译器与解释器（HIR / x86_64 native backend）",
    long_about = LONG_ABOUT,
    after_long_help = AFTER_LONG_HELP,
    arg_required_else_help = true,
    color = ColorChoice::Auto
)]
struct FullArgs {
    #[command(flatten)]
    core: CoreCli,
    #[command(flatten)]
    interp: InterpFlags,
    /// 运行模式
    #[arg(short, long, value_enum, default_value_t = RunMode::Interpret)]
    mode: RunMode,
    /// 编译目标平台（仅 compile 模式生效）
    #[arg(long, value_enum, default_value_t = CompileTarget::build_default())]
    target: CompileTarget,
}

#[derive(Parser, Debug)]
#[command(
    name = "bf-interpreter",
    version,
    about = "Brainfuck HIR 解释器",
    long_about = INTERP_LONG_ABOUT,
    after_long_help = INTERP_AFTER_HELP,
    arg_required_else_help = true,
    color = ColorChoice::Auto
)]
struct InterpreterArgs {
    #[command(flatten)]
    core: CoreCli,
    #[command(flatten)]
    interp: InterpFlags,
}

#[derive(Parser, Debug)]
#[command(
    name = "bf-compiler",
    version,
    about = "Brainfuck → 构建目标对应的 x86_64 原生编译器",
    long_about = COMPILER_LONG_ABOUT,
    after_long_help = COMPILER_AFTER_HELP,
    arg_required_else_help = true,
    color = ColorChoice::Auto
)]
struct CompilerArgs {
    #[command(flatten)]
    core: CoreCli,
    /// 编译目标平台；默认跟随构建本二进制时的目标平台，可通过该参数交叉编译
    #[arg(long, value_enum, default_value_t = CompileTarget::build_default())]
    target: CompileTarget,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub driver_cfg: DriverConfig,

    /// 0 -> quiet
    /// 1 -> normal
    /// 2 -> v
    /// 3 -> vv
    /// _ -> vvv
    pub log_level: u8,
}

fn load_source(input: &PathBuf) -> Result<String> {
    if input.as_os_str() == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        Ok(std::fs::read_to_string(input)?)
    }
}

fn quiet_requested() -> bool {
    std::env::args_os().any(|arg| arg == "-q" || arg == "--quiet")
}

fn handle_clap_error(err: clap::Error, quiet: bool) -> ! {
    if !quiet {
        let _ = err.print();
    }
    match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            std::process::exit(0);
        }
        _ => {
            std::process::exit(2);
        }
    }
}

fn finish_config(core: CoreCli, mode: RunMode, interp_debug: bool) -> Result<AppConfig> {
    let target = CompileTarget::build_default();
    finish_config_with_target(core, mode, target, interp_debug)
}

fn finish_config_with_target(
    core: CoreCli,
    mode: RunMode,
    target: CompileTarget,
    interp_debug: bool,
) -> Result<AppConfig> {
    let source = load_source(&core.input)?;

    if core.verbose > 3 {
        if !core.quiet {
            eprintln!("错误: 详细级别最多为 3（即 -vvv）");
        }
        std::process::exit(1);
    }

    Ok(AppConfig {
        driver_cfg: DriverConfig {
            input: core.input,
            source,
            mode,
            target,
            output: core
                .output
                .unwrap_or_else(|| PathBuf::from(target.default_output_name())),
            interp_debug,
            opt_level: core.opt_level,
        },
        log_level: if core.quiet { 0 } else { core.verbose + 1 },
    })
}

pub fn parse_cli() -> Result<AppConfig> {
    let quiet = quiet_requested();
    let args = match FullArgs::try_parse() {
        Ok(a) => a,
        Err(e) => handle_clap_error(e, quiet),
    };
    finish_config_with_target(args.core, args.mode, args.target, args.interp.interp_debug)
}

/// Fixed `RunMode::Interpret`; no `-m` / `--mode`.
pub fn parse_interpreter_cli() -> Result<AppConfig> {
    let quiet = quiet_requested();
    let mut args = match InterpreterArgs::try_parse() {
        Ok(a) => a,
        Err(e) => handle_clap_error(e, quiet),
    };
    if args.core.verbose == 0 {
        args.core.quiet = true;
    }
    finish_config(args.core, RunMode::Interpret, args.interp.interp_debug)
}

/// Fixed `RunMode::Compile`; no `-m` / `--mode` (and no `--interp-debug`).
pub fn parse_compiler_cli() -> Result<AppConfig> {
    let quiet = quiet_requested();
    let args = match CompilerArgs::try_parse() {
        Ok(a) => a,
        Err(e) => handle_clap_error(e, quiet),
    };
    finish_config_with_target(args.core, RunMode::Compile, args.target, false)
}
