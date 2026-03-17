/// Build script for writ-cli.
///
/// Embeds alpha build number into the version string when WRIT_ALPHA is set.
/// This is temporary for alpha testing — remove before release.
fn main() {
    let base = std::env::var("CARGO_PKG_VERSION").unwrap();
    let version = match std::env::var("WRIT_ALPHA") {
        Ok(a) if !a.is_empty() => format!("{}-alpha.{}", base, a),
        _ => base,
    };
    println!("cargo:rustc-env=WRIT_VERSION_FULL={}", version);
    // Only re-run if the alpha number changes.
    println!("cargo:rerun-if-env-changed=WRIT_ALPHA");
}
