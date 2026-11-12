fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("version") => {
            println!("bamm version {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!(
                "bamm: nothing implemented yet, see https://github.com/BaamStudios/bamm/issues"
            );
            std::process::exit(1);
        }
    }
}
