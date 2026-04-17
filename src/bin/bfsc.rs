fn main() {
    if let Err(e) = AmazingBF::run_bfsc() {
        eprintln!("bfsc: {e}");
        std::process::exit(1);
    }
}
