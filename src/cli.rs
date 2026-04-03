use crate::driver::config::{DriverConfig, RunMode};

use anyhow::Result;
use clap::{ArgAction, ColorChoice, Parser, error::ErrorKind};
use std::io::Read;
use std::path::PathBuf;

const LONG_ABOUT: &str = "\
AmazingBF 将 Brainfuck 源码走完整前端与中间层：词法/语法 → AST → HIR（可优化）→ LIR → \
（解释执行或 x86_64 后端）。\
默认在 HIR 上解释运行；compile 模式生成 Linux ELF，并在输出路径旁写入汇编与十六进制 listing。";

const AFTER_LONG_HELP: &str = "\
示例:
  # 解释执行（默认）
  AmazingBF path/to/hello.bf

  # 从标准输入读入源码
  cat hello.bf | AmazingBF -

  # 编译为可执行文件并运行
  AmazingBF path/to/hello.bf -m compile -o ./hello_bf
  ./hello_bf

  # 只跑通流水线并在日志里看各阶段规模（不写文件）
  AmazingBF path/to/hello.bf -m dump -vv

  # 解释执行并在 stderr 打印 tape 使用统计
  AmazingBF path/to/hello.bf --interp-debug

手册页（若已安装）: man amazingbf";

#[derive(Parser, Debug)]
#[command(
    name = "AmazingBF",
    version,
    about = "Brainfuck 编译器与解释器（HIR / Linux x86_64 ELF）",
    long_about = LONG_ABOUT,
    after_long_help = AFTER_LONG_HELP,
    arg_required_else_help = true,
    color = ColorChoice::Auto
)]
struct Args {
    /// Brainfuck 源文件路径；使用 `-` 表示从标准输入读取
    #[arg(value_name = "FILE")]
    input: PathBuf,

    /// 写入的 ELF 路径（仅 compile 模式；同时生成同基名的 .asm / .lst）
    #[arg(short, long, value_name = "PATH", default_value = "a.out")]
    output: PathBuf,

    /// 提高日志详细度（可重复：-v / -vv / -vvv）
    #[arg(short, long, action = ArgAction::Count, group = "log_level")]
    verbose: u8,

    /// 静默日志（与 -v 互斥）
    #[arg(short, long, action = ArgAction::SetTrue, group = "log_level")]
    quiet: bool,

    /// 运行模式
    #[arg(short, long, value_enum, default_value_t = RunMode::Interpret)]
    mode: RunMode,

    /// 解释执行结束后在 stderr 输出 tape 统计（指针范围、向右扩容、左右移动量等）
    #[arg(long = "interp-debug")]
    interp_debug: bool,
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

pub fn parse_cli() -> Result<AppConfig> {
    let quiet_requested = std::env::args_os().any(|arg| arg == "-q" || arg == "--quiet");

    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(err) => {
            if !quiet_requested {
                err.print()?;
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
    };

    let source = load_source(&args.input)?;

    if args.verbose > 3 {
        if !args.quiet {
            eprintln!("错误: 详细级别最多为 3（即 -vvv）");
        }
        std::process::exit(1);
    }

    Ok(AppConfig {
        driver_cfg: DriverConfig {
            input: args.input,
            source,
            mode: args.mode,
            output: args.output,
            interp_debug: args.interp_debug,
        },
        log_level: if args.quiet { 0 } else { args.verbose + 1 },
    })
}
