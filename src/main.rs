mod cli;
mod driver;
mod frontend;
mod ir;

fn main() {
    let config = match cli::parse_cli() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };

    println!("Source is {:?}", config.source);

    // if let Err(err) = driver::run::run(config) {
    //     eprintln!("{}", err);
    //     std::process::exit(1);
    // }
}
