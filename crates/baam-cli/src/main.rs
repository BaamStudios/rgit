fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("version") => {
            println!("baam version {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!(
                "baam: nothing implemented yet, see https://github.com/BaamStudios/baam/issues"
            );
            std::process::exit(1);
        }
    }
}
