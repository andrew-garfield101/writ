//! The agent workflow as one source (finding 67).
//!
//! Every generated instruction surface (the CLAUDE.md and AGENTS.md blocks,
//! `.writ/AGENT_INSTRUCTIONS.md`, the `.claude/settings.json` instruction,
//! the SessionStart hook text and the generated skills) takes its commands
//! and steps from here, so they cannot drift apart again.
//!
//! The commands are macros so `const` templates can `concat!` them.

/// Register and claim a task. Comma form for `--scope`, the form agents
/// write (finding 66); `--claim` so the next seal knows the spec is yours
/// (finding 87).
#[macro_export]
macro_rules! cmd_spec_add {
    () => {
        r#"writ spec add "brief description of your task" --scope "<files you will change, comma-separated>" --claim"#
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
        concat!(
            "writ spec add \"",
            $title,
            "\" --scope \"",
            $scope,
            "\" --claim"
        )
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

/// Finding 114: agents act on doctor's fixes, including fixes about other
/// agents' specs, unless the fix is marked for a human. The same sentence
/// on every surface (a workflow step, and the context and doctor skills).
#[macro_export]
macro_rules! doctor_note {
    () => {
        "When `writ context` or `writ doctor` shows a finding whose fix is not marked for a human, run that fix, including for findings about other agents' specs, because writ has already checked it is safe"
    };
}

pub const DOCTOR_NOTE: &str = doctor_note!();

pub const SPEC_ADD: &str = cmd_spec_add!();
pub const SEAL: &str = cmd_seal!();
pub const SPEC_DONE: &str = cmd_spec_done!();

/// Sprint 6 (0.4.3): what an agent under Claude Code still needs to know
/// when its edits are sealed by the hooks. The CLAUDE.md and AGENTS.md
/// blocks, the SessionStart echo and the settings.json instruction are
/// these lines and nothing else: no mandatory command, no identity
/// instruction (subagents get their own identity from the hook, finding
/// 131). Each is one line of markdown.
pub fn auto_steps() -> [String; 4] {
    [
        r#"This project uses writ. In Claude Code, writ seals the edits it can attribute and asks you with one line about the rest; in other tools, seal with `writ seal -s "<summary>" --paths <files>`. Leave git commits and `writ finish` to the user."#.to_string(),
        format!("`writ context` shows project state and other agents' work. When a task is done, `{SPEC_DONE}` records your own summary (optional)."),
        // Aubs, c409 transcripts: a subagent declined the ambiguity fix
        // because its brief said "run no writ commands"; the template says
        // the one line is meant to be run.
        "If writ cannot tell whose change a file is, it asks you with one line; run that command.".to_string(),
        format!("{DOCTOR_NOTE}."),
    ]
}

/// [`auto_steps`] as markdown lines.
pub fn auto_markdown() -> String {
    auto_steps().iter().map(|s| format!("{s}\n")).collect()
}

/// [`auto_steps`] as plain text for a shell `echo` (backticks dropped).
pub fn auto_plain() -> Vec<String> {
    auto_steps().iter().map(|s| s.replace('`', "")).collect()
}

/// The hand-run workflow, in order, for hosts with no auto seal hook
/// (`.writ/AGENT_INSTRUCTIONS.md`). Each step is one line of markdown.
pub fn workflow_steps() -> [String; 5] {
    [
        "BEFORE starting any work, run `writ context` to check project state".to_string(),
        format!("If no spec is assigned to you, create one: `{SPEC_ADD}`"),
        format!("AFTER each meaningful unit of work: `{SEAL}` (auto-scoped to your spec)"),
        format!("When the task is complete, BEFORE reporting results: `{SPEC_DONE}`"),
        DOCTOR_NOTE.to_string(),
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
pub const CLAIM_NOTE: &str = "`--claim` on `writ spec add` makes the spec yours; without it, your first seal claims the one spec you created. To take over an existing unclaimed spec instead, run `writ spec claim <id>`.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_steps_are_zero_instruction() {
        let all = auto_markdown();
        assert_eq!(all.lines().count(), 4);
        assert!(all.contains("it asks you with one line; run that command"));
        assert!(!all.contains("nothing to run"));
        for w in [
            "MANDATORY",
            "MUST",
            "EVERY TASK",
            "NOT complete",
            "BEFORE",
            "FIRST ACTION",
            "WRIT_AGENT_ID",
        ] {
            assert!(!all.contains(w), "{w}: {all}");
        }
        // Finding 146: the one seal command, scoped to tools without the hook.
        assert_eq!(all.matches("writ seal -s").count(), 1);
        assert!(all.contains("In Claude Code, writ seals the edits it can attribute and asks you with one line about the rest; in other tools, seal with"));
        assert!(!all.contains("sealed automatically"), "ruling 175: {all}");
        assert!(all.contains("writ context") && all.contains("writ spec done -s"));
        assert!(auto_plain().iter().all(|l| !l.contains('`')));
    }

    #[test]
    fn steps_use_the_comma_scope_and_a_done_summary() {
        let md = workflow_markdown();
        assert!(md.contains(r#"--scope "<files you will change, comma-separated>""#));
        assert!(md.contains(r#"writ spec done -s "<what you did>""#));
        assert!(md.starts_with("1. BEFORE"));
        assert!(workflow_plain().iter().all(|l| !l.contains('`')));
    }
}

#[cfg(test)]
mod doctor_note_tests {
    /// Finding 114: the doctor line is a workflow step, so every surface
    /// that renders the steps (and the parity test over them) carries it.
    #[test]
    fn doctor_note_is_a_step_with_the_agreed_wording() {
        assert!(super::DOCTOR_NOTE.contains("a finding whose fix is not marked for a human"));
        assert!(super::DOCTOR_NOTE.contains("other agents' specs"));
        assert!(super::DOCTOR_NOTE.contains("writ has already checked it is safe"));
        assert!(super::workflow_steps().contains(&super::DOCTOR_NOTE.to_string()));
        assert!(super::workflow_plain()
            .iter()
            .any(|l| l.contains("a finding whose fix is not marked for a human")));
    }
}

#[cfg(test)]
mod claim_tests {
    /// Finding 87: every surface's spec add claims.
    #[test]
    fn spec_add_forms_carry_claim() {
        assert!(super::SPEC_ADD.ends_with("--claim"));
        assert!(cmd_spec_add_with!("t", "a.rs").ends_with("--claim"));
        assert!(super::workflow_markdown().contains("--claim"));
        assert!(super::workflow_plain()
            .iter()
            .any(|l| l.contains("--claim")));
    }
}
