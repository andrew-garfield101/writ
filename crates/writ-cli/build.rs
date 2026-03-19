/// Build script for writ-cli.
///
/// Exposes CARGO_PKG_VERSION as WRIT_VERSION_FULL for compile-time embedding.
/// Version is controlled by the workspace Cargo.toml. Alpha/pre-release
/// numbering is handled by the release pipeline, not build-time env vars.
fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    println!("cargo:rustc-env=WRIT_VERSION_FULL={}", version);
}
