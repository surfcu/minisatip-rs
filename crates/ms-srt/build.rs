//! Links libsrt only for builds with the `srt` feature.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_SRT");
    if std::env::var_os("CARGO_FEATURE_SRT").is_some() {
        println!("cargo:rustc-link-lib=srt");
    }
}
