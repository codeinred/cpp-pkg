fn main() {
    if let Err(e) = cppkg::cli::run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
