//! Bakes build identity into the binary so `main.rs` can tell a genuinely
//! new build from a restart of the same one (see `changelog.rs`).
//!
//! Two env vars land in the compiled binary via `cargo:rustc-env`:
//! - `GIT_SHA`: short (7-char) commit sha of the build.
//! - `GIT_LOG`: subjects of the last 12 commits, one per record, each
//!   record `<sha>\x1e<subject>`, records joined by `\x1f`.
//!
//! The CI Docker build context for the bot is `bot/` alone (see
//! `.github/workflows/images.yml`), and that build has no `.git` at all —
//! so this script must never fail or panic when git is absent or errors
//! out. In that case it falls back to the `GIT_SHA_ARG`/`GIT_LOG_ARG`
//! build-args (populated by CI, see `bot/Dockerfile`), and if those are
//! unset too, to the literal "desconocido" / an empty log.

use std::path::PathBuf;
use std::process::Command;

/// Directory to run `git` from: the crate's parent dir, per spec, computed
/// from `CARGO_MANIFEST_DIR` rather than assumed cwd so it's correct
/// regardless of how the build script itself is invoked.
fn crate_parent_dir() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(&manifest_dir)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(manifest_dir))
}

/// Runs `git <args>` from the crate's parent dir. `None` on any failure:
/// git missing from PATH, no repo there, non-zero exit, non-UTF8 output —
/// all tolerated, never a build error.
fn git_output(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(crate_parent_dir()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn main() {
    let sha = git_output(&["rev-parse", "--short=7", "HEAD"])
        .or_else(|| std::env::var("GIT_SHA_ARG").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "desconocido".to_string());

    let log = git_output(&["log", "-12", "--format=%h%x1e%s"])
        .map(|s| s.lines().collect::<Vec<_>>().join("\u{1f}"))
        .or_else(|| std::env::var("GIT_LOG_ARG").ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();

    println!("cargo:rustc-env=GIT_SHA={sha}");
    println!("cargo:rustc-env=GIT_LOG={log}");

    // Emitting any rerun-if directive replaces cargo's default "rerun on
    // any package file change" heuristic entirely, so without care a local
    // `cargo build` after a plain `git commit` (no file besides HEAD/ref
    // changed) would keep embedding a stale sha. Watch `.git/HEAD` and,
    // since HEAD usually just points at a ref, the ref file it names too,
    // so both `git checkout` and `git commit` trigger a rerun. Neither
    // exists in the CI Docker build context (no `.git` there at all), in
    // which case this is a harmless no-op and every build is fresh anyway.
    let git_dir = crate_parent_dir().join(".git");
    let head = git_dir.join("HEAD");
    if let Ok(contents) = std::fs::read_to_string(&head) {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Some(ref_path) = contents.trim().strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed={}", git_dir.join(ref_path).display());
        }
    }
    println!("cargo:rerun-if-env-changed=GIT_SHA_ARG");
    println!("cargo:rerun-if-env-changed=GIT_LOG_ARG");
}
