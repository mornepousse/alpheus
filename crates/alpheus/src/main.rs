fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match alpheus::app::run::lire_options(&args) {
        Ok(options) => alpheus::app::run::run(options),
        Err(m) => {
            eprintln!("alpheus : {m}");
            std::process::exit(2);
        }
    }
}
