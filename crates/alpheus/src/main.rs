//! The `alpheus` program: reads the command line (`alpheus [--tab <name>]`) and runs the control
//! center. A bad option prints a message and exits with code 2; everything else is in the library
//! (`app::run`).

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match alpheus::app::run::parse_options(&args) {
        Ok(options) => alpheus::app::run::run(options),
        Err(m) => {
            eprintln!("alpheus: {m}");
            std::process::exit(2);
        }
    }
}
