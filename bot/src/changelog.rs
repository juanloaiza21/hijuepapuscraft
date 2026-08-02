//! Deploy changelog: when a genuinely new build of the bot starts, Don
//! Quijote proclaims once in the notify channel what changed since the
//! build last announced. Restarts of the *same* build (crash loops,
//! unattended-upgrade reboots, config reloads) stay silent.
//!
//! Build identity (`GIT_SHA`/`GIT_LOG`) is baked in at compile time by
//! `build.rs`; this module is the pure, testable half of the feature —
//! parsing that baked-in data, deciding whether to speak, and rendering
//! the proclamation. The impure half (reading/writing the "last
//! announced" state file, posting to Discord) lives in `main.rs`.

/// One build's identity: its own short sha, plus the subjects of the
/// commits `git log` saw at compile time (newest first), capped to the
/// last 12 by `build.rs`.
///
/// `shas` parallels `subjects` (the commit each subject belongs to) but
/// stays private: it exists only so `render` can find where a
/// previously-announced build sits in this build's own history, not as
/// part of the type's public contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub sha: String,
    pub subjects: Vec<String>,
    shas: Vec<String>,
}

/// Parses the two env strings `build.rs` bakes in.
///
/// `log_env` is zero or more records joined by `\x1f` (unit separator),
/// each record itself `<sha>\x1e<subject>` (record separator). Tolerant
/// of the ragged edges that can show up when git is missing at build time
/// or a build-arg fallback wasn't set: empty records are skipped, and a
/// record missing its `\x1e` still yields a (sha, empty-subject) pair
/// rather than panicking.
pub fn parse_build(sha_env: &str, log_env: &str) -> Build {
    let mut shas = Vec::new();
    let mut subjects = Vec::new();
    for record in log_env.split('\u{1f}') {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(2, '\u{1e}');
        let sha = parts.next().unwrap_or("").to_string();
        let subject = parts.next().unwrap_or("").to_string();
        shas.push(sha);
        subjects.push(subject);
    }
    Build { sha: sha_env.to_string(), subjects, shas }
}

/// Whether a genuinely new build has started, versus a restart of the one
/// already announced. `None` (nothing announced yet, e.g. fresh state
/// volume) always announces.
pub fn should_announce(current_sha: &str, last_announced: Option<&str>) -> bool {
    match last_announced {
        None => true,
        Some(prev) => prev != current_sha,
    }
}

/// Cap on how many commit subjects `render` lists before summarizing the
/// rest, per design.
const MAX_SUBJECTS: usize = 8;

/// First 7 chars of `sha` (its own short-sha length is already 7, but this
/// stays defensive against a longer or shorter value, e.g. the
/// "desconocido" fallback).
fn short_sha(sha: &str) -> &str {
    match sha.char_indices().nth(7) {
        Some((idx, _)) => &sha[..idx],
        None => sha,
    }
}

/// Renders the Quijote-voiced changelog proclamation for `build`.
///
/// If `last_announced` names a sha found among `build`'s own recent
/// history, only the subjects strictly newer than it are shown — so a
/// restart never re-lists what was already proclaimed. Otherwise (first
/// announcement ever, or the last-announced build has aged out of the
/// 12-commit window this build carries) the most recent commits are shown
/// instead. Either way the list is capped at [`MAX_SUBJECTS`], with a
/// closing note when anything was left out. Never panics, even with zero
/// subjects.
pub fn render(build: &Build, last_announced: Option<&str>) -> String {
    let cutoff = last_announced.and_then(|prev| build.shas.iter().position(|s| s == prev));
    let available: &[String] = match cutoff {
        Some(idx) => &build.subjects[..idx],
        None => &build.subjects[..],
    };
    let cap = available.len().min(MAX_SUBJECTS);
    let shown = &available[..cap];
    let truncated = available.len() > MAX_SUBJECTS;

    let mut out = String::new();
    out.push_str(
        ":scroll: ¡Oíd, oíd, buenas gentes de la ínsula! Don Quijote desmonta de Rocinante \
para pregonar la nueva singladura del código, recién forjada en la fragua de los \
desarrolladores desde la última crónica:\n",
    );
    if shown.is_empty() {
        out.push_str(
            "Ningún hecho memorable se ha hallado en los pergaminos, mas el código nunca cesa de mudar.\n",
        );
    } else {
        for subject in shown {
            let subject = subject.trim();
            let subject = if subject.is_empty() { "(sin asunto)" } else { subject };
            out.push_str("- ");
            out.push_str(subject);
            out.push('\n');
        }
        if truncated {
            out.push_str("...y algunas otras enmiendas menores.\n");
        }
    }
    out.push_str("\n*Que quede sellado en el pergamino:* build `");
    out.push_str(short_sha(&build.sha));
    out.push('`');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes `(sha, subject)` pairs the same way `build.rs` would, for
    /// feeding into `parse_build` in tests.
    fn encode(pairs: &[(&str, &str)]) -> String {
        pairs
            .iter()
            .map(|(sha, subject)| format!("{sha}\u{1e}{subject}"))
            .collect::<Vec<_>>()
            .join("\u{1f}")
    }

    #[test]
    fn parse_build_round_trips_normal_input() {
        let log = encode(&[("abc1234", "Fix the wobbly gate"), ("def5678", "Add new spell")]);
        let build = parse_build("abc1234", &log);
        assert_eq!(build.sha, "abc1234");
        assert_eq!(build.subjects, vec!["Fix the wobbly gate", "Add new spell"]);
    }

    #[test]
    fn parse_build_tolerates_empty_log() {
        let build = parse_build("abc1234", "");
        assert_eq!(build.sha, "abc1234");
        assert!(build.subjects.is_empty());
    }

    #[test]
    fn parse_build_tolerates_empty_sha() {
        let build = parse_build("", "abc1234\u{1e}Some subject");
        assert_eq!(build.sha, "");
        assert_eq!(build.subjects, vec!["Some subject"]);
    }

    #[test]
    fn parse_build_tolerates_malformed_records() {
        // A record missing its \x1e separator: no panic, empty subject.
        let build = parse_build("abc1234", "abc1234");
        assert_eq!(build.subjects, vec![""]);

        // Empty records (leading/trailing/doubled \x1f) are skipped, not
        // turned into spurious empty entries.
        let log = format!("\u{1f}{}\u{1f}\u{1f}", encode(&[("abc1234", "Real subject")]));
        let build = parse_build("abc1234", &log);
        assert_eq!(build.subjects, vec!["Real subject"]);
    }

    #[test]
    fn parse_build_never_panics_on_garbage() {
        for input in ["\u{1f}\u{1f}\u{1f}", "\u{1e}\u{1e}", "\u{1f}\u{1e}\u{1f}\u{1e}", "\0\0"] {
            let build = parse_build("x", input);
            // Just needs to not panic; content isn't meaningful here.
            let _ = render(&build, None);
        }
    }

    #[test]
    fn should_announce_matrix() {
        assert!(should_announce("abc1234", None));
        assert!(!should_announce("abc1234", Some("abc1234")));
        assert!(should_announce("abc1234", Some("def5678")));
    }

    #[test]
    fn render_includes_all_subjects_up_to_the_cap() {
        let pairs: Vec<(String, String)> = (0..5)
            .map(|i| (format!("sha{i}"), format!("Subject number {i}")))
            .collect();
        let encoded = encode(
            &pairs.iter().map(|(s, subj)| (s.as_str(), subj.as_str())).collect::<Vec<_>>(),
        );
        let build = parse_build("sha0", &encoded);
        let msg = render(&build, None);
        for (_, subject) in &pairs {
            assert!(msg.contains(subject), "missing subject in render: {subject}\n---\n{msg}");
        }
        assert!(!msg.contains("y algunas otras enmiendas menores"));
    }

    #[test]
    fn render_truncates_past_the_cap_and_says_so() {
        let pairs: Vec<(String, String)> = (0..12)
            .map(|i| (format!("sha{i}"), format!("Subject number {i}")))
            .collect();
        let encoded = encode(
            &pairs.iter().map(|(s, subj)| (s.as_str(), subj.as_str())).collect::<Vec<_>>(),
        );
        let build = parse_build("sha0", &encoded);
        let msg = render(&build, None);
        for (_, subject) in pairs.iter().take(MAX_SUBJECTS) {
            assert!(msg.contains(subject), "missing shown subject: {subject}");
        }
        for (_, subject) in pairs.iter().skip(MAX_SUBJECTS) {
            assert!(!msg.contains(subject), "unexpectedly showed truncated subject: {subject}");
        }
        assert!(msg.contains("y algunas otras enmiendas menores"));
    }

    #[test]
    fn render_only_shows_subjects_newer_than_last_announced() {
        // Newest first, as git log produces it.
        let pairs = [
            ("sha3", "Newest change"),
            ("sha2", "Middle change"),
            ("sha1", "Already announced change"),
            ("sha0", "Old change before that"),
        ];
        let encoded = encode(&pairs);
        let build = parse_build("sha3", &encoded);
        let msg = render(&build, Some("sha1"));
        assert!(msg.contains("Newest change"));
        assert!(msg.contains("Middle change"));
        assert!(!msg.contains("Already announced change"));
        assert!(!msg.contains("Old change before that"));
    }

    #[test]
    fn render_falls_back_to_recent_when_last_announced_not_in_history() {
        let pairs = [("sha2", "Newest change"), ("sha1", "Middle change"), ("sha0", "Old change")];
        let encoded = encode(&pairs);
        let build = parse_build("sha2", &encoded);
        // "sha-from-ages-ago" fell out of the 12-commit window this build
        // carries; falls back to showing the most recent commits.
        let msg = render(&build, Some("sha-from-ages-ago"));
        assert!(msg.contains("Newest change"));
        assert!(msg.contains("Middle change"));
        assert!(msg.contains("Old change"));
    }

    #[test]
    fn render_never_panics_and_leaves_no_placeholder_residue_on_empty_subjects() {
        let build = parse_build("abc1234", "");
        let msg = render(&build, None);
        assert!(!msg.contains('{') && !msg.contains('}'));
        assert!(msg.contains("abc1234"));

        let msg_with_prior = render(&build, Some("def5678"));
        assert!(!msg_with_prior.contains('{') && !msg_with_prior.contains('}'));
    }

    #[test]
    fn short_sha_caps_at_seven_chars_but_keeps_shorter_values() {
        assert_eq!(short_sha("abc1234extra"), "abc1234");
        assert_eq!(short_sha("abc12"), "abc12");
        assert_eq!(short_sha("desconocido"), "descono");
        assert_eq!(short_sha(""), "");
    }
}
