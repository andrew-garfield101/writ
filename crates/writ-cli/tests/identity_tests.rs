//! S.3 agent-identity, watch-track group (Bri). Complements Amis's
//! `identity.rs`: that file pins each behavior once; this one pins the
//! contract across every identity-taking command and the edges.
//!
//! - One resolver: every rung of explicit `--agent` > `WRIT_AGENT_ID` >
//!   `default_agent` > framework session > `human` resolves to the same id in
//!   `spec add`, `spec claim`, `spec release`, `seal` and `spec done`, with
//!   every lower rung present so precedence is exercised, not just presence.
//! - `spec release` by the holder and with `--force` (security log event).
//! - `spec add` claims only with `--claim`; the creator of an unclaimed spec
//!   counts as another agent at work for S.1 scope, `human` does not.
//! - `finish` never cancels zero-seal specs; `--archive-unclaimed` archives
//!   exactly the unclaimed zero-seal ones.
//! - `spec done` by a non-holder seals under the caller with a CLAIM warning
//!   (default) or is rejected with the spec left open (strict).
//!
//! Every test drives the real binary with the identity variables scrubbed, so
//! the host session (a Claude Code shell sets several) cannot leak in.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use writ_core::seal::Seal;
use writ_core::spec::{LifecycleState, Spec, SpecStatus};
use writ_core::Repository;

/// Every variable the resolver reads.
const IDENTITY_VARS: &[&str] = &[
    "WRIT_AGENT_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "ANTHROPIC_SESSION_ID",
    "CLAUDECODE",
    "CODEX_SESSION",
    "CODEX_SESSION_ID",
];

type Env<'a> = &'a [(&'a str, &'a str)];

struct Project(PathBuf);

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

impl Project {
    fn dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("writ-idb-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// `writ init --bare` with no git: enough for spec and seal commands.
    fn bare(tag: &str) -> Self {
        let p = Self(Self::dir(tag));
        p.ok(&[], &["init", "-y", "--bare"]);
        p
    }

    /// Git repo with one commit, `.writ/` ignored, then `writ init --bare`;
    /// what `finish` needs.
    fn with_git(tag: &str) -> Self {
        let p = Self(Self::dir(tag));
        p.git(&["init", "-q"]);
        p.git(&["config", "user.name", "writ-test"]);
        p.git(&["config", "user.email", "writ-test@localhost"]);
        p.write(".gitignore", ".writ/\n");
        p.write(".writignore", ".writ\n.git\n");
        p.write("README.md", "base\n");
        p.git(&["add", "-A"]);
        p.git(&["commit", "-q", "-m", "base"]);
        p.ok(&[], &["init", "-y", "--bare"]);
        p
    }

    fn write(&self, rel: &str, content: &str) {
        fs::write(self.0.join(rel), content).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
    }

    /// Run writ with exactly `env` among the identity variables.
    fn run(&self, env: Env, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_writ"));
        cmd.args(args).current_dir(&self.0).stdin(Stdio::null());
        for v in IDENTITY_VARS {
            cmd.env_remove(v);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn ok(&self, env: Env, args: &[&str]) -> String {
        let out = self.run(env, args);
        assert!(
            out.status.success(),
            "writ {args:?} env {env:?}: {}",
            text(&out)
        );
        text(&out)
    }

    fn repo(&self) -> Repository {
        Repository::open(&self.0).unwrap()
    }

    fn spec(&self, id: &str) -> Spec {
        self.repo().load_spec(id).unwrap()
    }

    /// Seals recorded on `spec`, oldest first (by `spec_id`, every branch;
    /// not `spec_log`, which also returns ancestor seals of other specs).
    fn seals(&self, spec: &str) -> Vec<Seal> {
        let mut v: Vec<Seal> = self
            .repo()
            .log_all()
            .unwrap()
            .into_iter()
            .filter(|s| s.spec_id.as_deref() == Some(spec))
            .collect();
        v.sort_by_key(|s| s.timestamp);
        v
    }

    fn events(&self) -> String {
        fs::read_to_string(self.0.join(".writ/security/events.jsonl")).unwrap_or_default()
    }
}

fn claim_warnings(seal: &Seal) -> Vec<&String> {
    seal.warnings
        .iter()
        .filter(|w| w.starts_with("CLAIM"))
        .collect()
}

// ---------------------------------------------------------------------------
// One resolver, every command, every rung
// ---------------------------------------------------------------------------

/// One rung of the ladder: the environment and setting that reach it (every
/// lower rung present too) and the id every command must resolve.
struct Rung {
    name: &'static str,
    flag: Option<&'static str>,
    env: &'static [(&'static str, &'static str)],
    default_agent: Option<&'static str>,
    expect: Expect,
}

enum Expect {
    Exact(&'static str),
    /// Framework ids carry a session hash; all commands must agree on it.
    Prefix(&'static str),
}

const LADDER: &[Rung] = &[
    Rung {
        name: "explicit --agent beats env, setting and framework",
        flag: Some("flagged"),
        env: &[
            ("WRIT_AGENT_ID", "from-env"),
            ("CLAUDE_CODE_SESSION_ID", "hub"),
        ],
        default_agent: Some("from-setting"),
        expect: Expect::Exact("flagged"),
    },
    Rung {
        name: "WRIT_AGENT_ID beats default_agent and framework (CC decision)",
        flag: None,
        env: &[
            ("WRIT_AGENT_ID", "from-env"),
            ("CLAUDE_CODE_SESSION_ID", "hub"),
        ],
        default_agent: Some("from-setting"),
        expect: Expect::Exact("from-env"),
    },
    Rung {
        name: "default_agent beats framework session",
        flag: None,
        env: &[("CLAUDE_CODE_SESSION_ID", "hub")],
        default_agent: Some("from-setting"),
        expect: Expect::Exact("from-setting"),
    },
    Rung {
        name: "blank WRIT_AGENT_ID falls through to default_agent",
        flag: None,
        env: &[("WRIT_AGENT_ID", "  "), ("CLAUDE_CODE_SESSION_ID", "hub")],
        default_agent: Some("from-setting"),
        expect: Expect::Exact("from-setting"),
    },
    Rung {
        name: "framework session when nothing above is set",
        flag: None,
        env: &[("CLAUDE_CODE_SESSION_ID", "hub")],
        default_agent: None,
        expect: Expect::Prefix("claude-code-"),
    },
    Rung {
        name: "human when nothing is set",
        flag: None,
        env: &[],
        default_agent: None,
        expect: Expect::Exact("human"),
    },
];

/// Walks one spec through add --claim, release, claim, seal and done under
/// a rung and returns the id each command recorded.
fn identities_under(rung: &Rung) -> Vec<(&'static str, String)> {
    let p = Project::bare("ladder");
    if let Some(d) = rung.default_agent {
        p.ok(&[], &["config", "set", "default_agent", d]);
    }
    let with_flag = |args: &[&'static str]| -> Vec<&'static str> {
        let mut v = args.to_vec();
        if let Some(f) = rung.flag {
            v.extend(["--agent", f]);
        }
        v
    };
    let env = rung.env;

    p.ok(
        env,
        &with_flag(&["spec", "add", "--id", "s", "--title", "s", "--claim"]),
    );
    let added = p.spec("s");
    let mut ids = vec![
        (
            "spec add created_by",
            added.created_by.clone().unwrap_or_default(),
        ),
        (
            "spec add --claim",
            added.claimed_by.clone().unwrap_or_default(),
        ),
    ];

    // Release succeeds only when it resolves the holder's id.
    p.ok(env, &with_flag(&["spec", "release", "s"]));
    assert_eq!(
        p.spec("s").claimed_by,
        None,
        "[{}] release kept the claim",
        rung.name
    );
    p.ok(env, &with_flag(&["spec", "claim", "s"]));
    ids.push(("spec claim", p.spec("s").claimed_by.unwrap_or_default()));

    p.write("a.txt", "one\n");
    p.ok(
        env,
        &with_flag(&["seal", "-s", "w", "--spec", "s", "--paths", "a.txt"]),
    );
    p.write("a.txt", "two\n");
    p.ok(
        env,
        &with_flag(&["spec", "done", "s", "-s", "d", "--paths", "a.txt"]),
    );

    let seals = p.seals("s");
    assert_eq!(
        seals.len(),
        2,
        "[{}] expected work seal + final seal",
        rung.name
    );
    for (label, seal) in [("seal", &seals[0]), ("spec done final seal", &seals[1])] {
        assert!(
            claim_warnings(seal).is_empty(),
            "[{}] {label} warned against its own claim: {:?}",
            rung.name,
            seal.warnings
        );
        ids.push((label, seal.agent.id.clone()));
    }
    assert_eq!(p.spec("s").status, SpecStatus::Complete, "[{}]", rung.name);
    ids
}

#[test]
fn every_rung_of_the_resolver_ladder_is_the_same_id_in_every_command() {
    for rung in LADDER {
        let ids = identities_under(rung);
        for (cmd, id) in &ids {
            match rung.expect {
                Expect::Exact(want) => {
                    assert_eq!(id, want, "[{}] {cmd} resolved {id}", rung.name)
                }
                Expect::Prefix(pre) => {
                    assert!(id.starts_with(pre), "[{}] {cmd} resolved {id}", rung.name)
                }
            }
        }
        let first = &ids[0].1;
        assert!(
            ids.iter().all(|(_, id)| id == first),
            "[{}] commands disagree: {ids:?}",
            rung.name
        );
    }
}

#[test]
fn claudecode_only_session_resolves_one_stable_id_across_invocations() {
    let p = Project::bare("claudecode");
    let env: Env = &[("CLAUDECODE", "1")];
    p.ok(
        env,
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );
    p.write("a.txt", "a\n");
    p.ok(env, &["seal", "-s", "w", "--spec", "s", "--paths", "a.txt"]);
    let seal = p.seals("s").remove(0);
    assert_eq!(Some(seal.agent.id.clone()), p.spec("s").claimed_by);
    assert!(claim_warnings(&seal).is_empty(), "{:?}", seal.warnings);
    p.ok(env, &["spec", "release", "s"]);
}

// ---------------------------------------------------------------------------
// spec release
// ---------------------------------------------------------------------------

#[test]
fn holder_release_is_not_logged_as_forced_and_frees_the_spec() {
    let p = Project::bare("rel-holder");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );

    p.ok(&[("WRIT_AGENT_ID", "ada")], &["spec", "release", "s"]);

    assert_eq!(p.spec("s").claimed_by, None);
    assert!(
        !p.events().contains("claim_force_released"),
        "{}",
        p.events()
    );
    p.ok(&[("WRIT_AGENT_ID", "bea")], &["spec", "claim", "s"]);
    assert_eq!(p.spec("s").claimed_by.as_deref(), Some("bea"));
}

#[test]
fn non_holder_release_is_refused_and_leaves_claim_and_log_untouched() {
    let p = Project::bare("rel-refused");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );

    let out = p.run(&[("WRIT_AGENT_ID", "bea")], &["spec", "release", "s"]);

    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("'ada'"),
        "refusal must name the holder: {}",
        text(&out)
    );
    assert_eq!(p.spec("s").claimed_by.as_deref(), Some("ada"));
    assert!(!p.events().contains("claim_force_released"));
}

#[test]
fn forced_release_clears_claim_and_logs_both_agents() {
    let p = Project::bare("rel-force");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );

    p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["spec", "release", "s", "--force"],
    );

    assert_eq!(p.spec("s").claimed_by, None);
    let ev = p
        .events()
        .lines()
        .find(|l| l.contains("claim_force_released"))
        .map(String::from)
        .unwrap_or_else(|| panic!("no claim_force_released event: {}", p.events()));
    let v: serde_json::Value = serde_json::from_str(&ev).unwrap();
    assert_eq!(v["agent_id"], "bea", "{ev}");
    let details = v["details"].as_str().unwrap();
    assert!(details.contains("'ada'") && details.contains("'s'"), "{ev}");
}

#[test]
fn release_of_an_unclaimed_spec_is_a_no_op_success() {
    let p = Project::bare("rel-unclaimed");
    p.ok(&[], &["spec", "add", "--id", "s", "--title", "s"]);
    p.ok(&[("WRIT_AGENT_ID", "bea")], &["spec", "release", "s"]);
    assert_eq!(p.spec("s").claimed_by, None);
    assert!(!p.events().contains("claim_force_released"));
}

#[test]
fn forced_release_under_strict_lets_the_new_holder_seal() {
    // The recovery path for a stale foreign claim (S.1 Changed item: stale
    // claims force --paths until S.3).
    let p = Project::bare("rel-strict");
    let cfg = p.0.join(".writ/config.toml");
    let c = fs::read_to_string(&cfg).unwrap();
    fs::write(
        &cfg,
        c.replacen(
            "[security]",
            "[security]\nclaim_enforcement = \"strict\"",
            1,
        ),
    )
    .unwrap();
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );
    p.write("a.txt", "a\n");
    let refused = p.run(
        &[("WRIT_AGENT_ID", "bea")],
        &["seal", "-s", "w", "--spec", "s", "--paths", "a.txt"],
    );
    assert!(!refused.status.success(), "{}", text(&refused));

    p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["spec", "release", "s", "--force"],
    );
    p.ok(&[("WRIT_AGENT_ID", "bea")], &["spec", "claim", "s"]);
    p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["seal", "-s", "w", "--spec", "s", "--paths", "a.txt"],
    );

    let seal = p.seals("s").remove(0);
    assert_eq!(seal.agent.id, "bea");
    assert!(claim_warnings(&seal).is_empty(), "{:?}", seal.warnings);
}

// ---------------------------------------------------------------------------
// spec add: no auto-claim; creator counts for S.1 scope
// ---------------------------------------------------------------------------

#[test]
fn unclaimed_spec_is_claimable_by_any_agent_and_claim_flag_blocks_others() {
    let p = Project::bare("add-claim");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "open", "--title", "o"],
    );
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "held", "--title", "h", "--claim"],
    );

    p.ok(&[("WRIT_AGENT_ID", "bea")], &["spec", "claim", "open"]);
    let denied = p.run(&[("WRIT_AGENT_ID", "bea")], &["spec", "claim", "held"]);

    assert_eq!(p.spec("open").claimed_by.as_deref(), Some("bea"));
    assert_eq!(p.spec("open").created_by.as_deref(), Some("ada"));
    assert!(!denied.status.success(), "{}", text(&denied));
    assert_eq!(p.spec("held").claimed_by.as_deref(), Some("ada"));
}

#[test]
fn creator_of_an_unclaimed_open_spec_counts_as_another_agent_for_default_scope() {
    let p = Project::bare("creator-counts");
    p.ok(
        &[("WRIT_AGENT_ID", "cat")],
        &["spec", "add", "--id", "planned", "--title", "p"],
    );
    p.ok(
        &[("WRIT_AGENT_ID", "dan")],
        &["spec", "add", "--id", "mine", "--title", "m", "--claim"],
    );
    p.write("x.txt", "x\n");

    let out = p.run(
        &[("WRIT_AGENT_ID", "dan")],
        &["seal", "-s", "w", "--spec", "mine"],
    );

    assert!(
        p.seals("mine").is_empty(),
        "unowned file swept: {}",
        text(&out)
    );
    let t = text(&out);
    assert!(
        t.contains("x.txt") && t.contains("cat"),
        "exclusion must name file and agent: {t}"
    );
    assert!(t.contains("--paths"), "{t}");
}

#[test]
fn human_creator_does_not_block_an_agents_first_default_seal() {
    let p = Project::bare("human-creator");
    p.ok(&[], &["spec", "add", "--id", "planned", "--title", "p"]);
    p.ok(
        &[("WRIT_AGENT_ID", "dan")],
        &["spec", "add", "--id", "mine", "--title", "m", "--claim"],
    );
    p.write("x.txt", "x\n");

    p.ok(
        &[("WRIT_AGENT_ID", "dan")],
        &["seal", "-s", "w", "--spec", "mine"],
    );

    let seal = p.seals("mine").remove(0);
    assert!(
        seal.changes.iter().any(|c| c.path == "x.txt"),
        "{:?}",
        seal.changes
    );
}

// ---------------------------------------------------------------------------
// finish never cancels; --archive-unclaimed archives exactly the unclaimed
// ---------------------------------------------------------------------------

/// Completes spec `id` as `ada` with one sealed file so finish has work.
fn complete_one(p: &Project, id: &str, file: &str) {
    let ada: Env = &[("WRIT_AGENT_ID", "ada")];
    p.ok(ada, &["spec", "add", "--id", id, "--title", id, "--claim"]);
    p.write(file, "w\n");
    p.ok(ada, &["seal", "-s", "w", "--spec", id, "--paths", file]);
    p.ok(ada, &["spec", "done", id, "-s", "d"]);
}

#[test]
fn repeated_finishes_never_cancel_zero_seal_specs_claimed_or_not() {
    // Finding 46: the second milestone finish cancelled them again.
    let p = Project::with_git("f46-repeat");
    p.ok(
        &[("WRIT_AGENT_ID", "cc")],
        &["spec", "add", "--id", "ahead", "--title", "a"],
    );
    p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["spec", "add", "--id", "held", "--title", "h", "--claim"],
    );

    complete_one(&p, "w1", "one.txt");
    p.ok(&[], &["finish", "-y"]);
    complete_one(&p, "w2", "two.txt");
    let second = p.ok(&[], &["finish", "-y"]);

    for id in ["ahead", "held"] {
        let s = p.spec(id);
        assert_eq!(s.lifecycle_state, LifecycleState::Active, "{id}: {second}");
        assert_eq!(s.status, SpecStatus::Pending, "{id}");
    }
    let listing = second
        .split("Open specs with no seals and no claim")
        .nth(1)
        .unwrap_or_else(|| panic!("no unclaimed listing: {second}"));
    let listed: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.trim().strip_prefix('·'))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert_eq!(listed, ["ahead"], "{second}");
}

#[test]
fn archive_unclaimed_archives_only_unclaimed_zero_seal_specs() {
    let p = Project::with_git("f46-flag");
    p.ok(&[], &["spec", "add", "--id", "idle", "--title", "i"]);
    p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["spec", "add", "--id", "held", "--title", "h", "--claim"],
    );
    // Sealed, then released: unclaimed but not zero-seal.
    p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["spec", "add", "--id", "worked", "--title", "w", "--claim"],
    );
    p.write("w.txt", "w\n");
    p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["seal", "-s", "w", "--spec", "worked", "--paths", "w.txt"],
    );
    p.ok(&[("WRIT_AGENT_ID", "bea")], &["spec", "release", "worked"]);
    complete_one(&p, "done1", "d.txt");

    let out = p.ok(&[], &["finish", "-y", "--archive-unclaimed"]);

    assert_eq!(
        p.spec("idle").lifecycle_state,
        LifecycleState::Cancelled,
        "{out}"
    );
    assert_eq!(
        p.spec("held").lifecycle_state,
        LifecycleState::Active,
        "{out}"
    );
    assert_eq!(
        p.spec("worked").lifecycle_state,
        LifecycleState::Active,
        "{out}"
    );
}

#[test]
fn finish_dry_run_with_archive_unclaimed_changes_no_spec() {
    let p = Project::with_git("f46-dry");
    p.ok(&[], &["spec", "add", "--id", "idle", "--title", "i"]);
    complete_one(&p, "done1", "d.txt");

    let out = p.run(&[], &["finish", "-y", "--dry-run", "--archive-unclaimed"]);

    assert_eq!(
        p.spec("idle").lifecycle_state,
        LifecycleState::Active,
        "{}",
        text(&out)
    );
}

// ---------------------------------------------------------------------------
// spec done by a non-holder
// ---------------------------------------------------------------------------

#[test]
fn non_holder_spec_done_seals_under_caller_with_claim_warning_naming_both() {
    let p = Project::bare("done-nonholder");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );
    p.write("a.txt", "a\n");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["seal", "-s", "w", "--spec", "s", "--paths", "a.txt"],
    );
    p.write("a.txt", "a2\n");

    p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["spec", "done", "s", "-s", "closing for ada"],
    );

    let seals = p.seals("s");
    let last = seals.last().unwrap();
    assert_eq!(seals.len(), 2, "no final seal");
    assert_eq!(
        last.agent.id, "bea",
        "final seal attributed to the holder, not the caller"
    );
    let w = claim_warnings(last);
    assert!(
        w.iter().any(|w| w.contains("'ada'") && w.contains("'bea'")),
        "{:?}",
        last.warnings
    );
    assert_eq!(p.spec("s").status, SpecStatus::Complete);
}

#[test]
fn non_holder_spec_done_under_strict_is_rejected_and_spec_stays_open() {
    let p = Project::bare("done-strict");
    let cfg = p.0.join(".writ/config.toml");
    let c = fs::read_to_string(&cfg).unwrap();
    fs::write(
        &cfg,
        c.replacen(
            "[security]",
            "[security]\nclaim_enforcement = \"strict\"",
            1,
        ),
    )
    .unwrap();
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );
    p.write("a.txt", "a\n");

    for args in [
        vec!["spec", "done", "s", "-s", "d", "--paths", "a.txt"],
        vec!["spec", "done", "s", "-s", "d"],
    ] {
        let out = p.run(&[("WRIT_AGENT_ID", "bea")], &args);
        assert!(!out.status.success(), "{args:?}: {}", text(&out));
        assert!(text(&out).contains("'ada'"), "{}", text(&out));
    }
    assert!(p.seals("s").is_empty(), "rejected done wrote a seal");
    let s = p.spec("s");
    assert_eq!(s.status, SpecStatus::Pending);
    assert_eq!(s.claimed_by.as_deref(), Some("ada"));
}

#[test]
fn non_holder_spec_done_without_a_seal_still_warns_about_the_claim() {
    let p = Project::bare("done-noseal");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );

    let out = p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["spec", "done", "s", "-s", "d"],
    );

    assert!(p.seals("s").is_empty());
    assert!(
        out.contains("CLAIM") && out.contains("ada"),
        "silent close of another agent's spec: {out}"
    );
}
