fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("version") => {
            println!("rgit version {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!(
                "rgit: nothing implemented yet, see https://github.com/BaamStudios/rgit/issues"
            );
            std::process::exit(1);
        }
    }
}
