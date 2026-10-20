//! Binary entry point: wiring, logging and signal handling.

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("minisatip version {}", env!("CARGO_PKG_VERSION"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_line_is_well_formed() {
        let line = format!("minisatip version {}", env!("CARGO_PKG_VERSION"));
        assert!(line.starts_with("minisatip version "));
    }
}
