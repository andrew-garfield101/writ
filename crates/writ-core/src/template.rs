//! The agent workflow as one source (finding 67).
//!
//! Every generated instruction surface (the CLAUDE.md and AGENTS.md blocks,
//! `.writ/AGENT_INSTRUCTIONS.md`, the `.claude/settings.json` instruction,
//! the SessionStart hook text and the generated skills) takes its commands
//! and steps from here, so they cannot drift apart again.
//!
//! The commands are macros so `const` templates can `concat!` them.

/// Register a task. Comma form for `--scope`, the form agents write
/// (finding 66).
#[macro_export]
macro_rules! cmd_spec_add {
    () => {
        r#"writ spec add "brief description of your task" --scope "<files you will change, comma-separated>""#
    };
}

/// Checkpoint work.
#[macro_export]
macro_rules! cmd_seal {
    () => {
        r#"writ seal -s "<summary>" --paths <changed files, comma-separated>"#
    };
}

/// Close the task, with a summary (finding 68).
#[macro_export]
macro_rules! cmd_spec_done {
    () => {
        r#"writ spec done -s "<what you did>""#
    };
}

/// `writ seal` with a fixed example summary, same `--paths` form.
#[macro_export]
macro_rules! cmd_seal_with {
    ($summary:literal) => {
        concat!(
            "writ seal -s \"",
            $summary,
            "\" --paths <changed files, comma-separated>"
        )
    };
}

/// `writ spec add` with an example title, same `--scope` form.
#[macro_export]
macro_rules! cmd_spec_add_with {
    ($title:literal, $scope:literal) => {
        concat!("writ spec add \"", $title, "\" --scope \"", $scope, "\"")
    };
}

/// What `--paths` means, the same sentence on every surface.
#[macro_export]
macro_rules! paths_note {
    () => {
        "`--paths` lists the files you changed (comma-separated). Without it, only files your spec owns are sealed (its `--scope` plus files its earlier seals captured); another agent's pending files are never included."
    };
}

pub const PATHS_NOTE: &str = paths_note!();

pub const SPEC_ADD: &str = cmd_spec_add!();
pub const SEAL: &str = cmd_seal!();
pub const SPEC_DONE: &str = cmd_spec_done!();

/// The required workflow, in order. Each step is one line of markdown.
pub fn workflow_steps() -> [String; 4] {
    [
        "BEFORE starting any work, run `writ context` to check project state".to_string(),
        format!("If no spec is assigned to you, create one: `{SPEC_ADD}`"),
        format!("AFTER each meaningful unit of work: `{SEAL}` (auto-scoped to your spec)"),
        format!("When the task is complete, BEFORE reporting results: `{SPEC_DONE}`"),
    ]
}

/// The steps as a numbered markdown list.
pub fn workflow_markdown() -> String {
    workflow_steps()
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {s}\n", i + 1))
        .collect()
}

/// The steps as plain text for a shell `echo` (backticks dropped).
pub fn workflow_plain() -> Vec<String> {
    workflow_steps()
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s.replace('`', "")))
        .collect()
}

/// How claims work, the same sentence everywhere.
pub const CLAIM_NOTE: &str = "`writ spec add` does not claim the spec; your first seal does, or pass `--claim`. To take over an existing unclaimed spec instead, run `writ spec claim <id>`.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_use_the_comma_scope_and_a_done_summary() {
        let md = workflow_markdown();
        assert!(md.contains(r#"--scope "<files you will change, comma-separated>""#));
        assert!(md.contains(r#"writ spec done -s "<what you did>""#));
        assert!(md.starts_with("1. BEFORE"));
        assert!(workflow_plain().iter().all(|l| !l.contains('`')));
    }
}
