fn main() {
    let mode = match wtr::cli::parse_args(std::env::args_os()) {
        Ok(mode) => mode,
        Err(e) => e.exit(),
    };
    let _ = mode;
}
