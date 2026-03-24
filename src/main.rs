mod cli;
mod driver;
mod frontend;
mod interp;
mod ir;
mod runtime;

#[cfg(all(feature = "llvm18", feature = "llvm22"))]
compile_error!("features `llvm18` and `llvm22` cannot be enabled at the same time");

fn main() {
    let config = match cli::parse_cli() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };

    println!("== SOURCE ==\n{:#?}", config.source);

    if let Err(err) = driver::run::run(config) {
        eprintln!("{}", err);
        std::process::exit(1);
    }
}
