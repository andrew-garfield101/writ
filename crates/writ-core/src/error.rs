//! Error types for writ operations.

use std::fmt;
use std::io;

/// All possible writ errors.
#[derive(Debug)]
pub enum WritError {
    /// The current directory is not a writ repository.
    NotARepo,
    /// A writ repository already exists here.
    AlreadyExists,
    /// An I/O error occurred.
    Io(io::Error),
    /// JSON serialization/deserialization failed.
    Json(serde_json::Error),
    /// An object with the given hash was not found.
    ObjectNotFound(String),
    /// No changes to seal.
    NothingToSeal,
    /// Pending changes exist, but none fall in the sealing spec's default
    /// scope. The scope lists each left-out file and why.
    NothingInScope {
        spec_id: String,
        scope: Box<crate::seal_scope::SealScope>,
    },
    /// A seal would remove lines that earlier seals of the same spec added
    /// and no later seal of that spec removed (finding 42). Sealing with
    /// `force` records the removal on purpose.
    OwnLinesRemoved {
        spec_id: String,
        losses: Vec<crate::convergence::survival::SurvivalLoss>,
        /// Agents other than the sealer who worked on the paths since the
        /// spec's last seal of them (finding 102); empty means nobody.
        other_agents: Vec<String>,
        /// Full ids of the spec's seals that added the lost lines.
        added_by: Vec<String>,
    },
    /// Ruling 181 (finding 145 closed): a seal would record the removal of
    /// lines another open spec sealed. Each entry: (other spec, the agent
    /// that sealed the lines, the seal that added them, path, line count).
    ForeignLinesRemoved {
        losses: Vec<(String, String, Option<String>, String, usize)>,
    },
    /// Ruling 181 (2): `spec done` found the closing identity's sealed lines
    /// missing from the files on disk. Nothing was closed.
    SpecDoneLinesLost {
        lost: usize,
        shown: Vec<String>,
        ms: u64,
    },
    /// Ruling 181 (2): the survival check for `spec done` did not finish
    /// inside its budget. Nothing was closed.
    SpecDoneSurvivalNotRun { budget_ms: u64 },
    /// Seal rejected under strict claim enforcement: the spec is claimed by
    /// a different agent.
    SealClaimConflict {
        spec_id: String,
        claimed_by: String,
        agent: String,
    },
    /// Finding 65: a seal or `spec done` on a spec already committed to git.
    /// Finish never stages it and the spec cannot be reopened, so it is
    /// refused before anything is written.
    SpecAlreadyCommitted {
        spec_id: String,
        title: String,
        commit_hash: Option<String>,
    },
    /// A seal with this ID was not found.
    SealNotFound(String),
    /// A spec with this ID was not found.
    SpecNotFound(String),
    /// A spec with this ID already exists.
    SpecAlreadyExists(String),
    /// Multiple specs match the given input (ambiguous slug or ID prefix).
    AmbiguousSpec {
        input: String,
        candidates: Vec<(String, String, String)>, // (id, slug, title)
    },
    /// A spec is already claimed by another agent.
    SpecAlreadyClaimed { spec_id: String, claimed_by: String },
    /// `writ spec release --stale` found no proof the holder is gone
    /// (finding 91). `reason` names the missing or contradicting evidence.
    StaleReleaseRefused {
        spec_id: String,
        holder: String,
        reason: String,
    },
    /// A seal with this ID already exists (immutable store).
    SealAlreadyExists(String),
    /// Spec has no seals — cannot converge.
    SpecHasNoSeals(String),
    /// Convergence has unresolved conflicts.
    UnresolvedConflicts(usize),
    /// Could not acquire the repository lock within the timeout.
    LockTimeout,
    /// No git repository found.
    NoGitRepo,
    /// A git operation failed.
    GitError(String),
    /// Bridge state is inconsistent.
    BridgeError(String),
    /// An agent with this ID was not found.
    AgentNotFound(String),
    /// An agent with this ID already exists.
    AgentAlreadyExists(String),
    /// Agent is revoked or suspended and cannot perform this action.
    AgentInactive(String),
    /// Agent tried to seal files outside their scope constraints.
    ScopeViolation(String),
    /// Path traversal or absolute path rejected.
    PathTraversal(String),
    /// Invalid hash format.
    InvalidHash(String),
    /// Input validation failure.
    InvalidInput(String),
    /// Attempted an illegal lifecycle state transition.
    InvalidLifecycleTransition(String),
    /// No remote configured with this name.
    RemoteNotFound(String),
    /// The remote directory is not a valid writ remote.
    InvalidRemote(String),
    /// A remote already exists with this name.
    RemoteAlreadyExists(String),
    /// Push rejected: remote HEAD has diverged.
    PushDiverged,
    /// Pull detected diverged histories.
    PullDiverged,
    /// Could not acquire the remote lock within the timeout.
    RemoteLockTimeout,
    /// Decompression bomb: object decompresses to more than the allowed limit.
    DecompressionBomb { hash: String, limit: usize },
    /// The GC live set could not be computed: these trees (one line each,
    /// `hash (root): reason`) could not be expanded, so pruning is unsafe.
    LiveSetIncomplete(Vec<String>),
    /// Generic error with a message.
    Other(String),
}

/// Lost lines shown per path in a refusal before "and N more".
const LOST_LINES_SHOWN: usize = 5;

impl WritError {
    /// Ruling 181: the one-line refusal for [`WritError::ForeignLinesRemoved`].
    /// The same text for the same state whether a hand seal or the auto
    /// seal hook was refused (Aubs's condition): the file, the other agent
    /// by name, its spec and seal, the lines it would remove, and the two
    /// exits, the pasteable one last after "fix:". None otherwise.
    pub fn foreign_lines_message(&self) -> Option<String> {
        let WritError::ForeignLinesRemoved { losses } = self else {
            return None;
        };
        let what: Vec<String> = losses
            .iter()
            .map(|(spec, by, seal, path, n)| {
                let seal = seal
                    .as_deref()
                    .map(|s| format!(", seal {}", &s[..s.len().min(12)]))
                    .unwrap_or_default();
                format!("{n} line(s) of {path} that agent {by} sealed (spec {spec}{seal})")
            })
            .collect();
        let mut paths: Vec<&str> = losses.iter().map(|l| l.3.as_str()).collect();
        paths.dedup();
        let p = paths.join(",");
        Some(format!(
            "writ did not seal {p}: it drops {}. Merge their lines back and seal again, \
             or keep your version and record the removal on purpose; \
             fix: writ seal --paths {p} --auto --allow-removals {p}",
            what.join(" and ")
        ))
    }

    /// Paths of an [`WritError::OwnLinesRemoved`] refusal, in order, deduped.
    pub fn own_lines_paths(&self) -> Vec<String> {
        let WritError::OwnLinesRemoved { losses, .. } = self else {
            return Vec::new();
        };
        let mut paths: Vec<String> = Vec::new();
        for l in losses {
            if !paths.contains(&l.path) {
                paths.push(l.path.clone());
            }
        }
        paths
    }

    /// The finding 102 refusal for [`WritError::OwnLinesRemoved`], with
    /// `override_cmd` (the caller's exact command plus `--allow-removals`)
    /// as the second choice. None for any other error.
    pub fn own_lines_message(&self, override_cmd: &str) -> Option<String> {
        let WritError::OwnLinesRemoved {
            spec_id,
            losses,
            other_agents,
            added_by,
        } = self
        else {
            return None;
        };
        let n: usize = losses.iter().map(|l| l.line_count()).sum();
        let paths = self.own_lines_paths().join(", ");
        let mut out = format!(
            "seal refused: {n} line(s) that spec '{spec_id}' sealed are no longer in {paths} on disk"
        );
        let quoted: Vec<String> = other_agents.iter().map(|a| format!("'{a}'")).collect();
        match crate::seal::join_names(&quoted) {
            Some(who) => out.push_str(&format!(
                ", and that part of the file was not written in this session: agent {who} worked on {paths} since your last seal of it. Nothing was sealed."
            )),
            None => out.push_str(&format!(
                ". No other agent has worked on {paths} since your last seal of it. Nothing was sealed."
            )),
        }
        for l in losses {
            out.push_str(&format!("\n  {}: line(s) {}", l.path, l.ranges_label()));
            for line in l.lines.iter().take(LOST_LINES_SHOWN) {
                out.push_str(&format!("\n    - {line}"));
            }
            if l.lines.len() > LOST_LINES_SHOWN {
                out.push_str(&format!(
                    "\n    and {} more",
                    l.lines.len() - LOST_LINES_SHOWN
                ));
            }
            if !l.foreign.is_empty() {
                out.push_str("\n    in their place, lines no seal recorded:");
                for line in &l.foreign {
                    out.push_str(&format!("\n    + {line}"));
                }
            }
        }
        let shows: Vec<String> = added_by
            .iter()
            .map(|id| format!("`writ show {} --diff`", &id[..id.len().min(12)]))
            .collect();
        let shows = if shows.is_empty() {
            String::new()
        } else {
            format!(" ({} shows them)", shows.join(", "))
        };
        out.push_str(&format!(
            "\nChoose one:\n  keep your lines: put them back in {paths} and seal again{shows}"
        ));
        match crate::seal::join_names(other_agents) {
            Some(who) => out.push_str(&format!(
                "\n  accept {who}'s version of {paths}: {override_cmd}"
            )),
            None => out.push_str(&format!("\n  remove them on purpose: {override_cmd}")),
        }
        Some(out)
    }
}

impl fmt::Display for WritError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WritError::NotARepo => write!(
                f,
                "not a writ repository (missing .writ/) — run `writ init` to initialize"
            ),
            WritError::AlreadyExists => write!(f, ".writ/ already exists"),
            WritError::Io(e) => write!(f, "I/O error: {e}"),
            WritError::Json(e) => write!(f, "JSON error: {e}"),
            WritError::ObjectNotFound(hash) => write!(f, "object not found: {hash}"),
            WritError::NothingToSeal => {
                write!(
                    f,
                    "no changes to seal — working directory matches last seal"
                )
            }
            WritError::SealNotFound(id) => write!(
                f,
                "seal not found: '{id}' — run `writ log` to see available seals"
            ),
            WritError::SpecNotFound(id) => write!(
                f,
                "spec not found: '{id}' — run `writ context` to see available specs"
            ),
            WritError::SpecAlreadyExists(id) => {
                write!(
                    f,
                    "spec '{id}' already exists (use 'writ spec update' to modify)"
                )
            }
            WritError::AmbiguousSpec { input, candidates } => {
                write!(f, "multiple specs match '{input}':")?;
                for (id, slug, title) in candidates {
                    write!(f, "\n  {id}  {slug}  \"{title}\"")?;
                }
                write!(f, "\nhint: use the full spec ID to disambiguate")
            }
            WritError::NothingInScope { spec_id, scope } => write!(
                f,
                "nothing to seal for spec '{spec_id}' — pending changes are outside its default scope ({}). Pass --paths to seal specific files",
                scope.left_out_summary()
            ),
            WritError::OwnLinesRemoved { .. } => {
                let paths = self.own_lines_paths().join(",");
                let cmd = format!("writ seal <same arguments> --allow-removals {paths}");
                f.write_str(&self.own_lines_message(&cmd).unwrap_or_default())
            }
            WritError::SpecDoneLinesLost { lost, shown, ms } => write!(
                f,
                "spec done refused: {lost} of your sealed line(s) are not in the files on disk ({}); merge them back and run spec done again. Nothing was closed. (survival check {ms} ms)",
                shown.join("; ")
            ),
            WritError::SpecDoneSurvivalNotRun { budget_ms } => write!(
                f,
                "spec done refused: survival not run (the check did not finish within {budget_ms} ms), so writ cannot say your sealed lines are intact. Nothing was closed. Run `writ doctor --survival`, then spec done again."
            ),
            WritError::ForeignLinesRemoved { .. } => f.write_str(
                &self.foreign_lines_message().unwrap_or_default(),
            ),
            WritError::SpecAlreadyCommitted {
                spec_id,
                title,
                commit_hash,
            } => {
                let at = commit_hash
                    .as_deref()
                    .map(|h| format!(" in {}", &h[..h.len().min(12)]))
                    .unwrap_or_default();
                let follow_up = format!("{title} (follow-up)").replace('"', "'");
                let id = crate::doctor::followup_spec_id(spec_id);
                write!(
                    f,
                    "spec '{spec_id}' is already committed to git{at}; finish would never stage a new seal on it. Continue this work on its follow-up spec:\n    writ spec add \"{follow_up}\" --id {id} --claim"
                )
            }
            WritError::SealClaimConflict {
                spec_id,
                claimed_by,
                agent,
            } => write!(
                f,
                "spec '{spec_id}' is claimed by agent '{claimed_by}'; agent '{agent}' cannot seal to it under strict claim enforcement ([security] claim_enforcement = \"strict\")"
            ),
            WritError::SpecAlreadyClaimed {
                spec_id,
                claimed_by,
            } => {
                write!(
                    f,
                    "spec '{spec_id}' is already claimed by agent '{claimed_by}'. Use `writ context` to find unclaimed specs."
                )
            }
            WritError::StaleReleaseRefused {
                spec_id,
                holder,
                reason,
            } => write!(
                f,
                "refusing to release spec '{spec_id}' from '{holder}' as stale: {reason}. \
                 --stale releases only when the holder is provably gone (same host, its pid \
                 exited or now belongs to a newer process). If you know '{holder}' is gone, \
                 a human may run `writ spec release {spec_id} --force` (recorded as forced)."
            ),
            WritError::SealAlreadyExists(id) => {
                write!(f, "seal '{id}' already exists (immutable)")
            }
            WritError::SpecHasNoSeals(id) => write!(
                f,
                "spec '{id}' has no seals — seal some work first with `writ seal --spec {id}`"
            ),
            WritError::UnresolvedConflicts(n) => {
                write!(
                    f,
                    "{n} unresolved conflict(s) — provide resolutions before applying"
                )
            }
            WritError::LockTimeout => write!(f, "could not acquire repository lock within timeout"),
            WritError::NoGitRepo => write!(f, "no git repository found"),
            WritError::GitError(msg) => write!(f, "git error: {msg}"),
            WritError::BridgeError(msg) => write!(f, "bridge error: {msg}"),
            WritError::AgentNotFound(id) => write!(f, "agent not found: {id}"),
            WritError::AgentAlreadyExists(id) => write!(f, "agent '{id}' already exists"),
            WritError::AgentInactive(id) => {
                write!(f, "agent '{id}' is revoked or suspended")
            }
            WritError::ScopeViolation(msg) => {
                write!(f, "scope violation: {msg}")
            }
            WritError::PathTraversal(path) => write!(f, "path rejected (traversal): {path}"),
            WritError::InvalidHash(hash) => write!(f, "invalid object hash: {hash}"),
            WritError::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            WritError::InvalidLifecycleTransition(msg) => {
                write!(f, "invalid lifecycle transition: {msg}")
            }
            WritError::RemoteNotFound(name) => write!(f, "remote not found: '{name}'"),
            WritError::InvalidRemote(path) => write!(f, "not a valid writ remote: {path}"),
            WritError::RemoteAlreadyExists(name) => {
                write!(f, "remote '{name}' already exists")
            }
            WritError::PushDiverged => {
                write!(f, "push rejected: remote HEAD has diverged — pull first")
            }
            WritError::PullDiverged => {
                write!(f, "histories diverged — use convergence to reconcile")
            }
            WritError::RemoteLockTimeout => {
                write!(f, "could not acquire remote lock within timeout")
            }
            WritError::DecompressionBomb { hash, limit } => {
                write!(
                    f,
                    "decompression bomb: object {} exceeds {} byte limit",
                    &hash[..hash.len().min(12)],
                    limit
                )
            }
            WritError::LiveSetIncomplete(trees) => write!(
                f,
                "live set incomplete, refusing to treat any object as garbage; {} unreadable tree(s): {}",
                trees.len(),
                trees.join("; ")
            ),
            WritError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for WritError {}

impl From<io::Error> for WritError {
    fn from(e: io::Error) -> Self {
        WritError::Io(e)
    }
}

impl From<serde_json::Error> for WritError {
    fn from(e: serde_json::Error) -> Self {
        WritError::Json(e)
    }
}

#[cfg(feature = "bridge")]
impl From<git2::Error> for WritError {
    fn from(e: git2::Error) -> Self {
        WritError::GitError(e.message().to_string())
    }
}

/// Convenience alias for Results in writ.
pub type WritResult<T> = Result<T, WritError>;
