fn main() {
    if let Err(err) = asterline::app::run() {
        eprintln!("Asterline failed: {err}");
        std::process::exit(1);
    }
}
