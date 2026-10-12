//! Findings 119 and 125: a possessive after a quoted name rendered as
//! `'name''s`. Every doctor, survival, seal and finish message is a format
//! string in these sources, so a placeholder followed by `''s` in any of
//! them reaches output in some state. Shell-escaped quotes (`'\''`) are a
//! different thing and are not matched.

use std::path::Path;

const SOURCES: [&str; 8] = [
    "../writ-core/src/doctor.rs",
    "../writ-core/src/convergence/survival.rs",
    "../writ-core/src/seal.rs",
    "../writ-core/src/error.rs",
    "../writ-core/src/handoff.rs",
    "../writ-core/src/why.rs",
    "src/finish.rs",
    "src/main.rs",
];

#[test]
fn no_message_renders_a_doubled_apostrophe_possessive() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut hits = Vec::new();
    for rel in SOURCES {
        let text = std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        for (n, line) in text.lines().enumerate() {
            // `'{x}''s` or `'{}''s`: a quoted placeholder, then `'s`.
            if line.contains("}''s") {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "doubled apostrophe possessive:\n{}",
        hits.join("\n")
    );
}
