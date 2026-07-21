fn main() {
    if let Ok(bundled) = std::env::var("MOMOR_BUNDLE") {
        println!("cargo:rustc-env=MOMOR_BUNDLE={}", bundled);
    }
}
