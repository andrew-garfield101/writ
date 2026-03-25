/// Build script for writ-cli.
///
/// Exposes WRIT_VERSION_FULL for compile-time embedding.
/// Base version comes from CARGO_PKG_VERSION (workspace Cargo.toml).
/// Alpha builds: set WRIT_ALPHA env var to append `-alpha.N` suffix.
/// No env var = clean version string (for release builds).
fn main() {
    let base = std::env::var("CARGO_PKG_VERSION").unwrap();
    let version = match std::env::var("WRIT_ALPHA") {
        Ok(a) if !a.is_empty() => format!("{}-alpha.{}", base, a),
        _ => base,
    };
    println!("cargo:rustc-env=WRIT_VERSION_FULL={}", version);
    println!("cargo:rerun-if-env-changed=WRIT_ALPHA");
}
