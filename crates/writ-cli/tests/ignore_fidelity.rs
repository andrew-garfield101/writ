//! ignore-fidelity (findings 19, 30) through the real binary: the global
//! excludes file and nested `.gitignore` files decide what `writ status`
//! and seals see, like git.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn writ(root: &Path, args: &[&str], excludes: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .current_dir(root)
        .env("WRIT_AGENT_ID", "human")
        .env("WRIT_EXCLUDES_FILE", excludes)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "writ {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Temp project removed on drop (writ-cli has no tempfile dev-dependency).
struct Dir(PathBuf);

impl Dir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn setup(tag: &str) -> Dir {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        Dir(std::env::temp_dir().join(format!("writ-ignore-{tag}-{}-{nanos}", std::process::id())));
    let root = dir.path();
    fs::create_dir_all(root.join("python/writ")).unwrap();
    fs::create_dir_all(root.join("bench/results")).unwrap();
    fs::write(root.join("python/writ/_native.abi3.so"), "\x7fELF\n").unwrap();
    fs::write(root.join("python/writ/__init__.py"), "x = 1\n").unwrap();
    fs::write(root.join("bench/results/.gitignore"), "*\n!.gitignore\n").unwrap();
    fs::write(root.join("bench/results/run.json"), "{}\n").unwrap();
    writ(root, &["init", "-y", "--bare"], "");
    dir
}

fn pending(root: &Path, excludes: &str) -> String {
    writ(root, &["diff", "--name-only"], excludes)
}

#[test]
fn global_excludes_file_hides_the_compiled_so() {
    let dir = setup("global");
    let global = dir.path().join("global_ignore");
    fs::write(&global, "*.so\n").unwrap();
    let with = pending(dir.path(), global.to_str().unwrap());
    assert!(!with.contains("_native.abi3.so"), "{with}");
    assert!(with.contains("python/writ/__init__.py"), "{with}");
}

#[test]
fn no_global_excludes_when_unset() {
    let dir = setup("unset");
    let without = pending(dir.path(), "");
    assert!(without.contains("_native.abi3.so"), "{without}");
}

#[test]
fn nested_results_gitignore_is_honored() {
    let dir = setup("nested");
    let out = pending(dir.path(), "");
    assert!(!out.contains("bench/results/run.json"), "{out}");
}
