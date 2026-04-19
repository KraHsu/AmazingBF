//! `bfsc` — BFS (Brainf Script) source-level compiler that lowers `.bfs` to Brainfuck.
//!
//! Owns the `bfsc` binary's top-level pipeline: argument parsing, I/O, and the
//! fixed sequence `lex → parse → typeck → codegen`. When `-c` is passed the
//! generated BF is handed to `driver::run` to produce a native executable;
//! otherwise it is written as BF source.

mod ast;
mod codegen;
mod layout;
mod lexer;
mod parser;
mod typeck;

use std::fmt;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Errors produced by the `bfsc` binary's compile pipeline.
#[derive(Debug)]
pub(crate) enum BfscError {
    /// Filesystem or stdin I/O failure.
    Io(std::io::Error),
    /// Lexer-stage failure with a human-readable diagnostic.
    Lex(String),
    /// Parser-stage failure with a human-readable diagnostic.
    Parse(String),
    /// Type-check failure with a human-readable diagnostic.
    Type(String),
    /// Backend compile failure when `-c` is passed.
    Compile(String),
}

impl fmt::Display for BfscError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BfscError::Io(e) => write!(f, "I/O error: {e}"),
            BfscError::Lex(m) => write!(f, "Lexer error: {m}"),
            BfscError::Parse(m) => write!(f, "Parse error: {m}"),
            BfscError::Type(m) => write!(f, "Type error: {m}"),
            BfscError::Compile(m) => write!(f, "Compile error: {m}"),
        }
    }
}

impl From<std::io::Error> for BfscError {
    fn from(e: std::io::Error) -> Self {
        BfscError::Io(e)
    }
}

struct ParsedArgs {
    source: String,
    output_path: Option<String>,
    compile: bool,
    target: crate::driver::config::CompileTarget,
    opt_level: crate::driver::config::OptLevel,
    quiet: bool,
}

/// Entry point of the `bfsc` binary: parse arguments, compile, and emit output.
pub(crate) fn run() -> Result<(), BfscError> {
    let args: Vec<String> = std::env::args().collect();
    let parsed = parse_args(&args)?;
    let bf = compile(&parsed.source)?;
    if parsed.compile {
        run_compile_mode(bf, &parsed)?;
    } else {
        match &parsed.output_path {
            Some(path) => std::fs::write(path, &bf)?,
            None => print!("{bf}"),
        }
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, BfscError> {
    use crate::driver::config::{CompileTarget, OptLevel};

    let mut input_path: Option<String> = None;
    let mut output_path: Option<String> = None;
    let mut compile = false;
    let mut target = CompileTarget::build_default();
    let mut opt_level = OptLevel::O3;
    let mut quiet = false;
    let mut i = 1usize;

    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                eprintln!("bfsc {VERSION}");
                std::process::exit(0);
            }
            "-c" | "--compile" => compile = true,
            "-q" | "--quiet" => quiet = true,
            "-o" | "--output" => {
                i += 1;
                output_path = Some(
                    args.get(i)
                        .ok_or_else(|| BfscError::Parse(format!("missing argument for {arg}")))?
                        .clone(),
                );
            }
            _ if arg.starts_with("--output=") => {
                output_path = Some(arg["--output=".len()..].to_owned());
            }
            "--target" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| BfscError::Parse("missing argument for --target".into()))?
                    .as_str();
                target = CompileTarget::parse(raw).ok_or_else(|| {
                    BfscError::Parse(format!(
                        "invalid compile target `{raw}` (expected x86_64-linux or x86_64-windows)"
                    ))
                })?;
            }
            _ if arg.starts_with("--target=") => {
                let raw = &arg["--target=".len()..];
                target = CompileTarget::parse(raw).ok_or_else(|| {
                    BfscError::Parse(format!(
                        "invalid compile target `{raw}` (expected x86_64-linux or x86_64-windows)"
                    ))
                })?;
            }
            "-O" | "--opt-level" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| BfscError::Parse(format!("missing argument for {arg}")))?
                    .as_str();
                opt_level = OptLevel::parse(raw).ok_or_else(|| {
                    BfscError::Parse(format!(
                        "invalid opt level `{raw}` (expected 0, 1, 2, or 3)"
                    ))
                })?;
            }
            _ if arg.starts_with("--opt-level=") => {
                let raw = &arg["--opt-level=".len()..];
                opt_level = OptLevel::parse(raw).ok_or_else(|| {
                    BfscError::Parse(format!(
                        "invalid opt level `{raw}` (expected 0, 1, 2, or 3)"
                    ))
                })?;
            }
            // -O0 / -O1 / -O2 / -O3 inline shorthand
            _ if arg.starts_with("-O") && arg.len() == 3 => {
                let raw = &arg[2..];
                opt_level = OptLevel::parse(raw).ok_or_else(|| {
                    BfscError::Parse(format!(
                        "invalid opt level `{raw}` (expected 0, 1, 2, or 3)"
                    ))
                })?;
            }
            "-" => input_path = Some("-".to_owned()),
            _ if !arg.starts_with('-') => {
                if input_path.is_some() {
                    return Err(BfscError::Parse("too many input files".into()));
                }
                input_path = Some(arg.to_owned());
            }
            other => {
                return Err(BfscError::Parse(format!("unknown flag: {other}")));
            }
        }
        i += 1;
    }

    let source = match input_path.as_deref() {
        Some("-") | None => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
        Some(path) => std::fs::read_to_string(path)?,
    };

    Ok(ParsedArgs {
        source,
        output_path,
        compile,
        target,
        opt_level,
        quiet,
    })
}

fn run_compile_mode(bf: String, args: &ParsedArgs) -> Result<(), BfscError> {
    use crate::driver::config::{DriverConfig, RunMode};
    use std::path::PathBuf;

    let output = args
        .output_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(args.target.default_output_name()));

    let log_level = if args.quiet { 0u8 } else { 1u8 };
    crate::logging::init_logger(log_level).map_err(|e| BfscError::Compile(e.to_string()))?;

    let config = DriverConfig {
        source: bf,
        mode: RunMode::Compile,
        target: args.target,
        output,
        interp_debug: false,
        opt_level: args.opt_level,
    };

    crate::driver::run::run(config).map_err(|e| BfscError::Compile(e.to_string()))
}

fn print_help() {
    eprintln!(
        "bfsc {VERSION}\n\
         BFS (Brainf Script) \u{2192} Brainfuck \u{7f16}译器\n\n\
         \u{7528}法:\n\
         \x20 bfsc [\u{9009}项] <FILE>\n\n\
         \u{53c2}数:\n\
         \x20 <FILE>  BFS \u{6e90}文件路径；`-` 表示从标准输入读取\n\n\
         \u{9009}项:\n\
         \x20 -h, --help              显示帮助\n\
         \x20 -V, --version           显示版本\n\
         \x20 -o, --output <PATH>     输出路径\n\
         \x20                           不加 -c: 输出 BF 文本到文件（默认 stdout）\n\
         \x20                           加 -c:  可执行文件路径（默认 a.out / a.exe）\n\
         \x20 -c, --compile           将 .bfs 直接编译为原生可执行文件\n\
         \x20     --target <T>        编译目标（仅 -c）: x86_64-linux | x86_64-windows\n\
         \x20 -O, --opt-level <0-3>   优化级别（仅 -c，默认 3）\n\
         \x20 -q, --quiet             静默日志（仅 -c）\n\n\
         示例:\n\
         \x20 # 将 .bfs 编译为 BF 文本（输出到 stdout）\n\
         \x20 bfsc foo.bfs\n\n\
         \x20 # 保存 BF 文本到文件\n\
         \x20 bfsc foo.bfs -o foo.bf\n\n\
         \x20 # 将 .bfs 直接编译为原生可执行文件\n\
         \x20 bfsc foo.bfs -c -o foo\n\n\
         \x20 # 指定目标平台\n\
         \x20 bfsc foo.bfs -c --target x86_64-linux -o foo\n\n\
         手册页（若已安装）: man bfsc"
    );
}

/// Run the BFS front-end (`lex → parse → typeck → codegen`) and return the BF source string.
pub(crate) fn compile(source: &str) -> Result<String, BfscError> {
    let tokens = lexer::tokenize(source)?;
    let stmts = parser::parse(&tokens)?;
    let (typed, layout) = typeck::check(&stmts)?;
    let bf = codegen::emit(&typed, &layout);
    Ok(bf)
}
