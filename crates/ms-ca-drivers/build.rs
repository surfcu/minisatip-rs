//! Links libdvbcsa when the `dvbcsa` feature is on.

fn main() {
    println!("cargo:rerun-if-env-changed=DVBCSA_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_DVBCSA").is_none() {
        return;
    }
    if let Some(dir) = std::env::var_os("DVBCSA_LIB_DIR") {
        println!("cargo:rustc-link-search=native={}", dir.to_string_lossy());
    }
    println!("cargo:rustc-link-lib=dvbcsa");
}
