//! Default seal scope: which pending files a spec-scoped seal captures.
//!
//! Writ cannot tell from the filesystem which agent edited a pending file,
//! so the default scope comes from the spec, not from the tree. A spec's
//! *own files* are its declared `file_scope` plus every path any of its
//! seals has captured. Each pending file is classified against the sealing
//! spec and every other open spec:
//!
//! - owned by the sealing spec: included (flagged `shared` when another
//!   open spec owns it too);
//! - owned only by another open spec: left out, attributed to that spec;
//! - owned by no open spec ("unowned"): included only for a regular seal,
//!   when the sealing spec declares no `file_scope` and no other agent holds
//!   an open claim. Otherwise left out with a hint to pass `--paths`.
//!
//! A final seal (`writ spec done`) never takes unowned files.

use std::collections::HashSet;

use serde::Serialize;

/// Which kind of seal the scope is computed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMode {
    /// `writ seal` without `--paths`.
    Seal,
    /// `writ spec done` without `--paths`: own files only, never unowned.
    Done,
}

/// Ownership data for one spec, used by [`classify`].
#[derive(Debug, Clone, Default)]
pub struct SpecOwnership {
    pub spec_id: String,
    /// Declared `file_scope` entries (paths, `dir/` prefixes, or globs).
    pub file_scope: Vec<String>,
    /// Every path captured by any of this spec's seals.
    pub sealed_paths: HashSet<String>,
}

impl SpecOwnership {
    /// True when `path` is one of this spec's own files.
    pub fn owns(&self, path: &str) -> bool {
        self.sealed_paths.contains(path) || path_in_scope(&self.file_scope, path)
    }

    /// True when the spec has no own files yet.
    pub fn is_empty(&self) -> bool {
        self.sealed_paths.is_empty() && self.file_scope.is_empty()
    }
}

/// Why an unowned pending file was left out of a seal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnownedReason {
    /// The sealing spec declares a `file_scope` and the file is outside it.
    OutsideFileScope,
    /// Another agent holds an open claim, so the file may be theirs.
    OtherAgentActive,
    /// Final seal (`spec done`) never takes unowned files.
    FinalSeal,
}

/// A pending file left out because another open spec owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnedElsewhere {
    pub path: String,
    pub spec_id: String,
}

/// A pending file included in the seal that another open spec also owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SharedFile {
    pub path: String,
    pub also_owned_by: Vec<String>,
}

/// An open spec claimed by an agent other than the one sealing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimHolder {
    pub spec_id: String,
    pub agent: String,
}

/// Finding 74: a tracked file whose newest spec seal is on a committed spec
/// while git HEAD does not hold that content. It matches the index, so no
/// seal sees it as changed, and finish skips committed specs, so nothing
/// commits it. The seal paths list it as pending (`Modified`, content
/// unchanged) so a new spec can take it; the fix is a seal of it under an
/// open spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StuckFile {
    pub path: String,
    /// The committed spec whose seal is the newest for the path.
    pub spec_id: String,
    pub seal_id: String,
}

/// Result of classifying the pending files for one seal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SealScope {
    /// Paths the seal captures, sorted.
    pub included: Vec<String>,
    /// Pending files left for other specs.
    pub other_specs: Vec<OwnedElsewhere>,
    /// Pending files owned by no open spec and left out.
    pub unowned: Vec<String>,
    /// Why unowned files were left out (None when there are none).
    pub unowned_reason: Option<UnownedReason>,
    /// Included files that another open spec also owns.
    pub shared: Vec<SharedFile>,
    /// Open claims by other agents, when they caused unowned files to be
    /// left out (`UnownedReason::OtherAgentActive`).
    pub claim_holders: Vec<ClaimHolder>,
}

impl SealScope {
    /// True when any pending file was left out of the seal.
    pub fn has_left_out(&self) -> bool {
        !self.other_specs.is_empty() || !self.unowned.is_empty()
    }

    /// Why unowned files were left out, as one phrase.
    pub fn unowned_reason_text(&self) -> String {
        match self.unowned_reason {
            Some(UnownedReason::OutsideFileScope) => {
                "outside this spec's declared file_scope".to_string()
            }
            Some(UnownedReason::OtherAgentActive) => {
                let holders: Vec<String> = self
                    .claim_holders
                    .iter()
                    .map(|h| format!("'{}' on spec {}", h.agent, h.spec_id))
                    .collect();
                format!(
                    "not yet owned by any spec and another agent holds an open claim ({})",
                    holders.join(", ")
                )
            }
            Some(UnownedReason::FinalSeal) => {
                "not one of this spec's files; a final seal takes only files the spec owns"
                    .to_string()
            }
            None => "unowned".to_string(),
        }
    }

    /// One line per left-out file: `path: reason`.
    pub fn left_out_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .other_specs
            .iter()
            .map(|o| format!("{}: owned by spec {}", o.path, o.spec_id))
            .collect();
        let why = self.unowned_reason_text();
        lines.extend(self.unowned.iter().map(|p| format!("{p}: {why}")));
        lines
    }

    /// Human-readable description of what was left out, for errors.
    pub fn left_out_summary(&self) -> String {
        self.left_out_lines().join("; ")
    }

    /// Left-out files the sealing agent may have written: the unowned ones.
    /// Files owned by another open spec are never suggested.
    pub fn retry_paths(&self) -> &[String] {
        &self.unowned
    }

    /// The command to run to seal the left-out unowned files, or None when
    /// none were left out.
    ///
    /// When no other agent holds an open claim the unowned files can only be
    /// the caller's, so the paths are filled in. When another agent does,
    /// some of them may be that agent's work: filling them in would make a
    /// verbatim paste sweep it (the 13a failure), so the line carries a
    /// `<paths>` placeholder and the candidates are listed above it.
    pub fn retry_line(
        &self,
        summary: &str,
        spec_id: &str,
        agent: &str,
        done: bool,
    ) -> Option<String> {
        if self.unowned.is_empty() {
            return None;
        }
        let paths: &[String] = if self.claim_holders.is_empty() {
            &self.unowned
        } else {
            &[]
        };
        Some(retry_command(summary, spec_id, agent, paths, done))
    }

    /// True when [`Self::retry_line`] carries the `<paths>` placeholder.
    pub fn retry_needs_paths(&self) -> bool {
        !self.unowned.is_empty() && !self.claim_holders.is_empty()
    }

    /// Machine-readable hints for bindings: one `LEFT_OUT: path: reason`
    /// line per left-out file, then the paste-ready command when there is
    /// one. `done` selects the `writ spec done` form of the command.
    pub fn hint_lines(&self, summary: &str, spec_id: &str, agent: &str, done: bool) -> Vec<String> {
        let mut hints: Vec<String> = self
            .left_out_lines()
            .into_iter()
            .map(|line| format!("LEFT_OUT: {line}"))
            .collect();
        if let Some(line) = self.retry_line(summary, spec_id, agent, done) {
            let lead = if self.retry_needs_paths() {
                "Another agent is working here; replace <paths> with the files you changed and run"
            } else {
                "If you changed these files, seal them with"
            };
            hints.push(format!("{lead}: {line}"));
        }
        hints
    }
}

/// Placeholder an agent replaces with its own comma-separated paths.
pub const PATHS_PLACEHOLDER: &str = "<paths>";

/// Classify pending paths for a seal on `this` spec.
///
/// `others` holds every *other open* spec. `other_claims` lists the open
/// specs claimed by a different agent.
pub fn classify(
    pending: &[String],
    this: &SpecOwnership,
    others: &[SpecOwnership],
    other_claims: &[ClaimHolder],
    mode: ScopeMode,
) -> SealScope {
    let mut scope = SealScope::default();
    let other_agent_active = !other_claims.is_empty();
    let unowned_reason = match mode {
        ScopeMode::Done => Some(UnownedReason::FinalSeal),
        ScopeMode::Seal if !this.file_scope.is_empty() => Some(UnownedReason::OutsideFileScope),
        ScopeMode::Seal if other_agent_active => Some(UnownedReason::OtherAgentActive),
        ScopeMode::Seal => None,
    };

    for path in pending {
        let owners: Vec<String> = others
            .iter()
            .filter(|o| o.owns(path))
            .map(|o| o.spec_id.clone())
            .collect();
        if this.owns(path) {
            scope.included.push(path.clone());
            if !owners.is_empty() {
                scope.shared.push(SharedFile {
                    path: path.clone(),
                    also_owned_by: owners,
                });
            }
        } else if let Some(first) = owners.into_iter().next() {
            scope.other_specs.push(OwnedElsewhere {
                path: path.clone(),
                spec_id: first,
            });
        } else if unowned_reason.is_none() {
            scope.included.push(path.clone());
        } else {
            scope.unowned.push(path.clone());
        }
    }

    if !scope.unowned.is_empty() {
        scope.unowned_reason = unowned_reason;
        scope.claim_holders = other_claims.to_vec();
    }
    scope.included.sort();
    scope.unowned.sort();
    scope.other_specs.sort_by(|a, b| a.path.cmp(&b.path));
    scope.shared.sort_by(|a, b| a.path.cmp(&b.path));
    scope
}

/// True when `path` matches any `file_scope` entry.
///
/// Entries ending in `/` match as directory prefixes, entries containing `*`
/// as globs, and anything else as an exact path or directory prefix.
pub fn path_in_scope(file_scope: &[String], path: &str) -> bool {
    // Finding 66: an entry stored as one comma-separated string (specs
    // created before the CLI split them) still matches per item.
    split_scope_entries(file_scope).iter().any(|scope| {
        if let Some(dir) = scope.strip_suffix('/') {
            path.starts_with(scope) || path == dir
        } else if scope.contains('*') {
            crate::ignore::glob_match(scope, path)
        } else {
            path == scope || path.starts_with(&format!("{scope}/"))
        }
    })
}

/// Split `--scope` values on commas (finding 66): agents write
/// `--scope "a.py,b.py,tests/*"`, and the repeated flag still works. Items
/// are trimmed; empty items dropped. Globs here have no `{a,b}` braces, so a
/// comma is never part of a pattern.
pub fn split_scope_entries(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|e| e.split(','))
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(String::from)
        .collect()
}

/// A paste-ready command that seals `paths` explicitly.
///
/// `done` selects `writ spec done` instead of `writ seal`. Every argument is
/// shell-quoted when needed, so the line can be run as printed. Empty
/// `paths` yields the [`PATHS_PLACEHOLDER`] for the agent to fill in.
pub fn retry_command(
    summary: &str,
    spec_id: &str,
    agent: &str,
    paths: &[String],
    done: bool,
) -> String {
    let paths = if paths.is_empty() {
        PATHS_PLACEHOLDER.to_string()
    } else {
        shell_quote(&paths.join(","))
    };
    let summary = shell_quote(summary);
    let spec = shell_quote(spec_id);
    let agent = shell_quote(agent);
    if done {
        format!("writ spec done {spec} -s {summary} --agent {agent} --paths {paths}")
    } else {
        format!("writ seal -s {summary} --spec {spec} --agent {agent} --paths {paths}")
    }
}

/// Quote `s` for a POSIX shell unless it only has safe characters.
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ',' | ':' | '@')
        });
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, scope: &[&str], sealed: &[&str]) -> SpecOwnership {
        SpecOwnership {
            spec_id: id.to_string(),
            file_scope: scope.iter().map(|s| s.to_string()).collect(),
            sealed_paths: sealed.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn comma_separated_scope_is_split_and_matches_each_item() {
        let one = vec!["app.py, models.py,tests/*,".to_string()];
        assert_eq!(
            split_scope_entries(&one),
            vec!["app.py", "models.py", "tests/*"]
        );
        assert!(path_in_scope(&one, "models.py"));
        assert!(path_in_scope(&one, "tests/test_app.py"));
        assert!(!path_in_scope(&one, "other.py"));
    }

    fn busy() -> Vec<ClaimHolder> {
        vec![ClaimHolder {
            spec_id: "c".into(),
            agent: "bob".into(),
        }]
    }

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn solo_agent_first_seal_takes_all_pending() {
        let this = spec("a", &[], &[]);
        let s = classify(&paths(&["x.rs", "y.rs"]), &this, &[], &[], ScopeMode::Seal);
        assert_eq!(s.included, paths(&["x.rs", "y.rs"]));
        assert!(!s.has_left_out());
    }

    #[test]
    fn solo_agent_later_seal_still_takes_new_files() {
        let this = spec("a", &[], &["x.rs"]);
        let s = classify(
            &paths(&["x.rs", "new.rs"]),
            &this,
            &[],
            &[],
            ScopeMode::Seal,
        );
        assert_eq!(s.included, paths(&["new.rs", "x.rs"]));
    }

    #[test]
    fn files_owned_by_other_open_spec_are_left_out() {
        let this = spec("a", &[], &["x.rs"]);
        let other = spec("b", &[], &["y.rs"]);
        let s = classify(
            &paths(&["x.rs", "y.rs"]),
            &this,
            &[other],
            &busy(),
            ScopeMode::Seal,
        );
        assert_eq!(s.included, paths(&["x.rs"]));
        assert_eq!(
            s.other_specs,
            vec![OwnedElsewhere {
                path: "y.rs".into(),
                spec_id: "b".into()
            }]
        );
    }

    #[test]
    fn unowned_left_out_when_other_agent_active() {
        let this = spec("a", &[], &[]);
        let s = classify(&paths(&["z.rs"]), &this, &[], &busy(), ScopeMode::Seal);
        assert!(s.included.is_empty());
        assert_eq!(s.unowned, paths(&["z.rs"]));
        assert_eq!(s.unowned_reason, Some(UnownedReason::OtherAgentActive));
    }

    #[test]
    fn declared_file_scope_excludes_unowned_outside_it() {
        let this = spec("a", &["src/"], &[]);
        let s = classify(
            &paths(&["src/a.rs", "docs/b.md"]),
            &this,
            &[],
            &[],
            ScopeMode::Seal,
        );
        assert_eq!(s.included, paths(&["src/a.rs"]));
        assert_eq!(s.unowned, paths(&["docs/b.md"]));
        assert_eq!(s.unowned_reason, Some(UnownedReason::OutsideFileScope));
    }

    #[test]
    fn file_scope_union_sealed_paths_are_own_files() {
        let this = spec("a", &["src/"], &["README.md"]);
        let s = classify(
            &paths(&["src/a.rs", "README.md"]),
            &this,
            &[],
            &[],
            ScopeMode::Seal,
        );
        assert_eq!(s.included, paths(&["README.md", "src/a.rs"]));
    }

    #[test]
    fn shared_file_included_and_flagged() {
        let this = spec("a", &[], &["x.rs"]);
        let other = spec("b", &[], &["x.rs"]);
        let s = classify(&paths(&["x.rs"]), &this, &[other], &busy(), ScopeMode::Seal);
        assert_eq!(s.included, paths(&["x.rs"]));
        assert_eq!(s.shared[0].also_owned_by, vec!["b".to_string()]);
    }

    #[test]
    fn done_mode_never_takes_unowned() {
        let this = spec("a", &[], &["x.rs"]);
        let s = classify(&paths(&["x.rs", "z.rs"]), &this, &[], &[], ScopeMode::Done);
        assert_eq!(s.included, paths(&["x.rs"]));
        assert_eq!(s.unowned, paths(&["z.rs"]));
        assert_eq!(s.unowned_reason, Some(UnownedReason::FinalSeal));
    }

    #[test]
    fn done_mode_with_no_own_files_takes_nothing() {
        let this = spec("a", &[], &[]);
        let s = classify(&paths(&["z.rs"]), &this, &[], &[], ScopeMode::Done);
        assert!(s.included.is_empty());
    }

    #[test]
    fn path_in_scope_matches_dirs_globs_and_exact() {
        let scope = paths(&["src/", "lib", "*.md"]);
        assert!(path_in_scope(&scope, "src/a.rs"));
        assert!(path_in_scope(&scope, "lib/b.rs"));
        assert!(path_in_scope(&scope, "lib"));
        assert!(path_in_scope(&scope, "README.md"));
        assert!(!path_in_scope(&scope, "library/x.rs"));
        assert!(!path_in_scope(&scope, "srcx/a.rs"));
    }

    #[test]
    fn left_out_summary_names_specs_and_reason() {
        let this = spec("a", &[], &[]);
        let other = spec("b", &[], &["y.rs"]);
        let s = classify(
            &paths(&["y.rs", "z.rs"]),
            &this,
            &[other],
            &busy(),
            ScopeMode::Seal,
        );
        let msg = s.left_out_summary();
        assert!(msg.contains("y.rs: owned by spec b"));
        assert!(msg.contains("'bob' on spec c"));
        assert_eq!(s.retry_paths(), &["z.rs".to_string()]);
    }

    #[test]
    fn retry_command_is_paste_ready_and_quoted() {
        let cmd = retry_command(
            "fix it's bug",
            "auth",
            "agent-1",
            &paths(&["src/a.rs", "my file.rs"]),
            false,
        );
        assert_eq!(
            cmd,
            "writ seal -s 'fix it'\\''s bug' --spec auth --agent agent-1 --paths 'src/a.rs,my file.rs'"
        );
        let done = retry_command("done", "auth", "a", &paths(&["x.rs"]), true);
        assert_eq!(done, "writ spec done auth -s done --agent a --paths x.rs");
    }

    #[test]
    fn retry_line_fills_paths_only_without_other_claims() {
        let this = spec("a", &["src/"], &[]);
        let solo = classify(&paths(&["docs/x.md"]), &this, &[], &[], ScopeMode::Seal);
        assert_eq!(
            solo.retry_line("s", "a", "me", false).unwrap(),
            "writ seal -s s --spec a --agent me --paths docs/x.md"
        );
        assert!(!solo.retry_needs_paths());

        let crowded = classify(&paths(&["docs/x.md"]), &this, &[], &busy(), ScopeMode::Seal);
        let line = crowded.retry_line("s", "a", "me", false).unwrap();
        assert!(line.ends_with("--paths <paths>"), "{line}");
        assert!(crowded.retry_needs_paths());
        assert_eq!(crowded.claim_holders, busy());
    }

    #[test]
    fn retry_line_is_none_when_only_other_specs_files_left_out() {
        let this = spec("a", &[], &["x.rs"]);
        let other = spec("b", &[], &["y.rs"]);
        let s = classify(&paths(&["y.rs"]), &this, &[other], &busy(), ScopeMode::Seal);
        assert!(s.retry_line("s", "a", "me", false).is_none());
    }

    #[test]
    fn hint_lines_list_left_out_files_before_the_retry_command() {
        let this = spec("a", &[], &["x.rs"]);
        let other = spec("b", &[], &["y.rs"]);
        // y.rs is b's and is left out; docs/x.md is unowned and, with another
        // claim present, needs --paths.
        let s = classify(
            &paths(&["y.rs", "docs/x.md"]),
            &this,
            &[other],
            &busy(),
            ScopeMode::Seal,
        );
        let hints = s.hint_lines("s", "a", "me", false);
        assert!(hints.len() >= 2, "{hints:?}");
        let last = hints.last().unwrap();
        assert!(last.starts_with("Another agent is working here"), "{last}");
        assert!(last.ends_with("--paths <paths>"), "{last}");
        assert!(
            hints[..hints.len() - 1]
                .iter()
                .all(|h| h.starts_with("LEFT_OUT: ")),
            "{hints:?}"
        );
        assert!(hints.iter().any(|h| h.contains("y.rs")), "{hints:?}");
        let done = s.hint_lines("s", "a", "me", true);
        assert!(done.last().unwrap().contains("spec done"), "{done:?}");
    }
}
