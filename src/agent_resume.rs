use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_SESSION_ID_LEN: usize = 512;
const MAX_SESSION_PATH_LEN: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionRef {
    pub kind: AgentSessionRefKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRefKind {
    Id,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResumePlan {
    pub agent: String,
    pub argv: Vec<String>,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedAgentSession {
    pub source: String,
    pub agent: String,
    pub session_ref: AgentSessionRef,
    /// Where the agent keeps this session's native conversation transcript,
    /// as reported by its integration. Informational only: it never takes
    /// part in deciding whether two records name the same session.
    pub transcript_path: Option<PathBuf>,
}

/// The native transcript files behind a persisted agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTranscript {
    /// The conversation transcript itself.
    pub file: PathBuf,
    /// A directory of per-session side data (tool results, subagent
    /// transcripts) kept next to the file; it may not exist.
    pub side_dir: Option<PathBuf>,
}

impl PersistedAgentSession {
    /// Whether both records name the same native session, ignoring the
    /// informational transcript path.
    pub fn same_session(&self, other: &Self) -> bool {
        self.source == other.source
            && self.agent == other.agent
            && self.session_ref == other.session_ref
    }

    /// The same record with `transcript_path` filled from `path` when it was
    /// still unknown.
    pub fn with_transcript_path(mut self, path: Option<PathBuf>) -> Self {
        if self.transcript_path.is_none() {
            self.transcript_path = path;
        }
        self
    }
}

impl AgentSessionRef {
    pub fn id(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_session_id(&value).then_some(Self {
            kind: AgentSessionRefKind::Id,
            value,
        })
    }

    pub fn path(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_session_path(&value).then_some(Self {
            kind: AgentSessionRefKind::Path,
            value,
        })
    }
}

pub fn session_ref_from_report(
    source: &str,
    agent: &str,
    agent_session_id: Option<String>,
    _agent_session_path: Option<String>,
) -> Option<AgentSessionRef> {
    if !is_official_agent_source(source, agent) {
        return None;
    }

    if agent == "pi" || agent == "omp" {
        return _agent_session_path
            .and_then(AgentSessionRef::path)
            .or_else(|| agent_session_id.and_then(AgentSessionRef::id));
    }

    agent_session_id.and_then(AgentSessionRef::id)
}

/// The transcript path an integration reported next to an Id-kind session
/// reference. Path-kind references already are the transcript; other reports
/// carry no usable path. Only a path shaped like the agent's own transcript
/// location for that session id is kept (see [`is_native_transcript_path`]):
/// the path decides where a backup is restored to, so an arbitrary reported
/// path must not become a write target.
pub fn transcript_path_from_report(
    source: &str,
    agent: &str,
    session_ref: Option<&AgentSessionRef>,
    agent_session_path: Option<&str>,
) -> Option<PathBuf> {
    if !is_official_agent_source(source, agent) {
        return None;
    }
    let session_ref = session_ref?;
    if session_ref.kind != AgentSessionRefKind::Id {
        return None;
    }
    let path = agent_session_path?;
    if !valid_session_path(path) {
        return None;
    }
    let path = PathBuf::from(path);
    is_native_transcript_path(&path, &session_ref.value).then_some(path)
}

/// Whether `path` has the shape of a native transcript for session `id`:
/// `<config dir>/projects/<project-slug>/<id>.jsonl`, as Claude Code stores
/// them. The config directory is not pinned to `~/.claude` because Claude
/// honours `CLAUDE_CONFIG_DIR`. Reported, snapshotted, and backed-up paths
/// all pass through this check before Herdr reads from or writes to them.
pub fn is_native_transcript_path(path: &Path, id: &str) -> bool {
    path.is_absolute()
        && path.file_name().and_then(OsStr::to_str) == Some(format!("{id}.jsonl").as_str())
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            == Some(OsStr::new("projects"))
}

/// Where the agent behind `session` keeps its native transcript, when Herdr
/// knows how that agent stores conversations.
///
/// Claude Code stores `~/.claude/projects/<project-slug>/<session-id>.jsonl`
/// plus a `<session-id>/` side directory next to it. A reported transcript
/// path wins when it has that shape and the file exists; otherwise the
/// project directories are searched for the id. A well-formed but missing
/// path is still returned when the search finds nothing, so a backup can be
/// put back in place; a malformed path is ignored.
pub fn native_transcript_locations(session: &PersistedAgentSession) -> Option<NativeTranscript> {
    let home = crate::integration::home_dir().ok()?;
    native_transcript_locations_in(session, &home)
}

pub fn native_transcript_locations_in(
    session: &PersistedAgentSession,
    home: &Path,
) -> Option<NativeTranscript> {
    if !is_official_agent_source(&session.source, &session.agent) {
        return None;
    }
    match (
        session.source.as_str(),
        session.agent.as_str(),
        session.session_ref.kind,
    ) {
        ("herdr:claude", "claude", AgentSessionRefKind::Id) => {
            let id = &session.session_ref.value;
            if !is_safe_path_component(id) {
                return None;
            }
            let reported = session
                .transcript_path
                .clone()
                .filter(|path| is_native_transcript_path(path, id));
            let file = match reported {
                Some(path) if path.is_file() => path,
                // A stale reported path must not hide a transcript that
                // moved; a known-but-missing one stays the restore target.
                Some(path) => find_claude_transcript(home, id).unwrap_or(path),
                None => find_claude_transcript(home, id)?,
            };
            let side_dir = file.parent().map(|parent| parent.join(id));
            Some(NativeTranscript { file, side_dir })
        }
        _ => None,
    }
}

fn find_claude_transcript(home: &Path, id: &str) -> Option<PathBuf> {
    let projects = home.join(".claude").join("projects");
    let mut project_dirs: Vec<PathBuf> = std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    project_dirs.sort();
    project_dirs
        .into_iter()
        .map(|dir| dir.join(format!("{id}.jsonl")))
        .find(|candidate| candidate.is_file())
}

/// A session id or agent label that can be used as a single path component
/// without escaping the directory it is joined onto.
pub fn is_safe_path_component(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn persisted_session_from_launch_args(
    agent: crate::detect::Agent,
    args: &[String],
) -> Option<PersistedAgentSession> {
    let [command, session_id] = args else {
        return None;
    };
    if agent != crate::detect::Agent::Codex || command != "resume" || session_id.starts_with('-') {
        return None;
    }

    Some(PersistedAgentSession {
        source: "herdr:codex".into(),
        agent: "codex".into(),
        session_ref: AgentSessionRef::id(session_id.clone())?,
        transcript_path: None,
    })
}

pub fn normalize_session_start_source(value: Option<String>) -> Option<String> {
    match value.as_deref().map(str::trim) {
        Some(
            source @ ("startup" | "resume" | "clear" | "compact" | "branch" | "new" | "fork"
            | "select"),
        ) => Some(source.to_string()),
        _ => None,
    }
}

pub fn is_reserved_native_state_source(source: &str, agent: &str) -> bool {
    matches!(
        (source, agent),
        ("herdr:claude", "claude")
            | ("herdr:codex", "codex")
            | ("herdr:copilot", "copilot")
            | ("herdr:devin", "devin")
            | ("herdr:droid", "droid")
            | ("herdr:qodercli", "qodercli")
            | ("herdr:qwen", "qwen")
            | ("herdr:cursor", "cursor")
            | ("herdr:grok", "grok")
    )
}

pub fn session_ref_from_snapshot(
    source: &str,
    agent: &str,
    kind: AgentSessionRefKind,
    value: &str,
) -> Option<PersistedAgentSession> {
    if !is_official_agent_source(source, agent) {
        return None;
    }
    let session_ref = match (agent, kind) {
        ("pi" | "omp", AgentSessionRefKind::Path) => AgentSessionRef::path(value)?,
        (_, AgentSessionRefKind::Id) => AgentSessionRef::id(value)?,
        _ => return None,
    };
    Some(PersistedAgentSession {
        source: source.to_string(),
        agent: agent.to_string(),
        session_ref,
        transcript_path: None,
    })
}

pub fn plan(source: &str, agent: &str, session_ref: &AgentSessionRef) -> Option<AgentResumePlan> {
    if !is_official_agent_source(source, agent) {
        return None;
    }

    let argv = match (source, agent, session_ref.kind) {
        ("herdr:claude", "claude", AgentSessionRefKind::Id) => {
            vec![
                "claude".into(),
                "--resume".into(),
                session_ref.value.clone(),
            ]
        }
        ("herdr:codex", "codex", AgentSessionRefKind::Id) => {
            vec!["codex".into(), "resume".into(), session_ref.value.clone()]
        }
        ("herdr:copilot", "copilot", AgentSessionRefKind::Id) => {
            vec!["copilot".into(), format!("--resume={}", session_ref.value)]
        }
        ("herdr:devin", "devin", AgentSessionRefKind::Id) => {
            vec!["devin".into(), "--resume".into(), session_ref.value.clone()]
        }
        ("herdr:droid", "droid", AgentSessionRefKind::Id) => {
            vec!["droid".into(), "--resume".into(), session_ref.value.clone()]
        }
        ("herdr:kimi", "kimi", AgentSessionRefKind::Id) => {
            vec!["kimi".into(), "--session".into(), session_ref.value.clone()]
        }
        ("herdr:mastracode", "mastracode", AgentSessionRefKind::Id) => {
            vec![
                "mastracode".into(),
                "--thread".into(),
                session_ref.value.clone(),
            ]
        }
        ("herdr:pi", "pi", AgentSessionRefKind::Path | AgentSessionRefKind::Id) => {
            vec!["pi".into(), "--session".into(), session_ref.value.clone()]
        }
        ("herdr:omp", "omp", AgentSessionRefKind::Path | AgentSessionRefKind::Id) => {
            // omp resume is `-r, --resume=<value>` (ID prefix or path); it has no
            // `--session` flag, unlike pi.
            vec!["omp".into(), format!("--resume={}", session_ref.value)]
        }
        ("herdr:hermes", "hermes", AgentSessionRefKind::Id) => {
            vec![
                "hermes".into(),
                "--resume".into(),
                session_ref.value.clone(),
            ]
        }
        ("herdr:opencode", "opencode", AgentSessionRefKind::Id) => {
            vec![
                "opencode".into(),
                "--session".into(),
                session_ref.value.clone(),
            ]
        }
        ("herdr:qodercli", "qodercli", AgentSessionRefKind::Id) => {
            vec![
                "qodercli".into(),
                "--resume".into(),
                session_ref.value.clone(),
            ]
        }
        ("herdr:qwen", "qwen", AgentSessionRefKind::Id) => {
            vec!["qwen".into(), "--resume".into(), session_ref.value.clone()]
        }
        ("herdr:kilo", "kilo", AgentSessionRefKind::Id) => {
            vec!["kilo".into(), "--session".into(), session_ref.value.clone()]
        }
        ("herdr:cursor", "cursor", AgentSessionRefKind::Id) => {
            vec![
                if cfg!(windows) {
                    "cursor-agent.cmd"
                } else {
                    "cursor-agent"
                }
                .into(),
                "--resume".into(),
                session_ref.value.clone(),
            ]
        }
        ("herdr:antigravity_cli", "agy", AgentSessionRefKind::Id) => {
            vec![
                "agy".into(),
                "--conversation".into(),
                session_ref.value.clone(),
            ]
        }
        ("herdr:grok", "grok", AgentSessionRefKind::Id) => {
            vec!["grok".into(), "--resume".into(), session_ref.value.clone()]
        }
        ("herdr:letta", "letta", AgentSessionRefKind::Id) => {
            if let Some(agent_id) = session_ref.value.strip_prefix("default:") {
                if agent_id.is_empty() {
                    return None;
                }
                vec![
                    "letta".into(),
                    "--conversation".into(),
                    "default".into(),
                    "--agent".into(),
                    agent_id.into(),
                ]
            } else {
                vec![
                    "letta".into(),
                    "--conversation".into(),
                    session_ref.value.clone(),
                ]
            }
        }
        _ => return None,
    };

    Some(AgentResumePlan {
        agent: agent.to_string(),
        argv,
        dedupe_key: dedupe_key(source, agent, session_ref),
    })
}

/// Input that asks a running agent to exit gracefully from its own prompt.
///
/// Only agents with a verified in-app exit command are listed; an agent
/// without an entry cannot be suspended because Herdr has no way to stop it
/// without losing its native session. The text is submitted like a prompt
/// (text, then delayed Enter), so it must be a complete slash command.
pub fn graceful_exit_input(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("/exit"),
        _ => None,
    }
}

pub fn dedupe_key(source: &str, agent: &str, session_ref: &AgentSessionRef) -> String {
    format!(
        "{source}\u{0}{agent}\u{0}{:?}\u{0}{}",
        session_ref.kind, session_ref.value
    )
}

pub(crate) fn is_official_agent_source(source: &str, agent: &str) -> bool {
    matches!(
        (source, agent),
        ("herdr:claude", "claude")
            | ("herdr:codex", "codex")
            | ("herdr:copilot", "copilot")
            | ("herdr:devin", "devin")
            | ("herdr:droid", "droid")
            | ("herdr:kimi", "kimi")
            | ("herdr:omp", "omp")
            | ("herdr:mastracode", "mastracode")
            | ("herdr:pi", "pi")
            | ("herdr:hermes", "hermes")
            | ("herdr:opencode", "opencode")
            | ("herdr:qodercli", "qodercli")
            | ("herdr:qwen", "qwen")
            | ("herdr:kilo", "kilo")
            | ("herdr:cursor", "cursor")
            | ("herdr:antigravity_cli", "agy")
            | ("herdr:grok", "grok")
            | ("herdr:letta", "letta")
    )
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_SESSION_ID_LEN && !value.chars().any(char::is_control)
}

fn valid_session_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_PATH_LEN
        && !value.chars().any(char::is_control)
        && Path::new(value).is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute_test_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    struct TempHome(PathBuf);

    impl TempHome {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "herdr-agent-resume-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn claude_session(id: &str, transcript_path: Option<PathBuf>) -> PersistedAgentSession {
        PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id(id).unwrap(),
            transcript_path,
        }
    }

    #[test]
    fn native_transcript_path_shape_is_projects_slug_and_session_id() {
        // Absolute on every platform, unlike a literal `/home/...`.
        let cwd = std::env::current_dir().unwrap();
        let projects = cwd.join("projects");
        let ok = projects.join("-home-user-repo").join("id-1.jsonl");
        assert!(is_native_transcript_path(&ok, "id-1"));
        // Any config dir works: Claude honours CLAUDE_CONFIG_DIR.
        assert!(is_native_transcript_path(
            &cwd.join("claude-config")
                .join("projects")
                .join("slug")
                .join("id-1.jsonl"),
            "id-1"
        ));
        assert!(is_native_transcript_path(
            &cwd.join(".claude")
                .join("projects")
                .join("slug")
                .join("id-1.jsonl"),
            "id-1"
        ));
        for (path, id) in [
            // Another session's file.
            (ok.clone(), "id-2"),
            // Wrong extension or no extension.
            (projects.join("slug").join("id-1.json"), "id-1"),
            (projects.join("slug").join("id-1"), "id-1"),
            // `projects` is the parent, not the grandparent.
            (projects.join("id-1.jsonl"), "id-1"),
            // No `projects` directory.
            (
                std::env::current_dir()
                    .unwrap()
                    .join("elsewhere")
                    .join("slug")
                    .join("id-1.jsonl"),
                "id-1",
            ),
            (PathBuf::from("/etc/cron.d/id-1.jsonl"), "id-1"),
            // Relative paths.
            (PathBuf::from("projects/slug/id-1.jsonl"), "id-1"),
        ] {
            assert!(!is_native_transcript_path(&path, id), "{}", path.display());
        }
    }

    #[test]
    fn transcript_path_is_kept_only_for_official_id_refs_with_native_shape() {
        let transcript = absolute_test_path("projects/slug/claude-session.jsonl");
        let claude_ref = AgentSessionRef::id("claude-session").unwrap();
        assert_eq!(
            transcript_path_from_report(
                "herdr:claude",
                "claude",
                Some(&claude_ref),
                Some(&transcript)
            ),
            Some(PathBuf::from(&transcript))
        );
        assert_eq!(
            transcript_path_from_report(
                "custom:claude",
                "claude",
                Some(&claude_ref),
                Some(&transcript)
            ),
            None
        );
        assert_eq!(
            transcript_path_from_report("herdr:claude", "claude", None, Some(&transcript)),
            None
        );
        assert_eq!(
            transcript_path_from_report("herdr:claude", "claude", Some(&claude_ref), None),
            None
        );
        assert_eq!(
            transcript_path_from_report(
                "herdr:claude",
                "claude",
                Some(&claude_ref),
                Some("relative/projects/slug/claude-session.jsonl")
            ),
            None
        );
        // An absolute path without the transcript shape would become a
        // restore target: dropped, the projects glob takes over.
        for reported in [
            absolute_test_path("claude-session.jsonl"),
            absolute_test_path("projects/claude-session.jsonl"),
            absolute_test_path("projects/slug/other-session.jsonl"),
            "/etc/cron.d/claude-session.jsonl".to_string(),
        ] {
            assert_eq!(
                transcript_path_from_report(
                    "herdr:claude",
                    "claude",
                    Some(&claude_ref),
                    Some(&reported)
                ),
                None,
                "{reported}"
            );
        }
        let pi_ref = AgentSessionRef::path(absolute_test_path("pi-session.jsonl")).unwrap();
        assert_eq!(
            transcript_path_from_report("herdr:pi", "pi", Some(&pi_ref), Some(&transcript)),
            None
        );
    }

    #[test]
    fn same_session_ignores_the_transcript_path_while_equality_does_not() {
        let without = claude_session("claude-session", None);
        let with = claude_session("claude-session", Some(PathBuf::from("/tmp/a.jsonl")));
        assert!(without.same_session(&with));
        assert_ne!(without, with);
        assert!(!without.same_session(&claude_session("other", None)));
        assert_eq!(
            without
                .clone()
                .with_transcript_path(Some(PathBuf::from("/tmp/a.jsonl"))),
            with
        );
        // A known path is not replaced.
        assert_eq!(
            with.clone()
                .with_transcript_path(Some(PathBuf::from("/tmp/b.jsonl"))),
            with
        );
    }

    #[test]
    fn native_transcript_uses_the_reported_path_even_when_the_file_is_gone() {
        let home = TempHome::new("reported");
        let file = home.0.join("projects").join("slug").join("id-1.jsonl");
        let session = claude_session("id-1", Some(file.clone()));
        assert_eq!(
            native_transcript_locations_in(&session, &home.0),
            Some(NativeTranscript {
                side_dir: Some(file.parent().unwrap().join("id-1")),
                file,
            })
        );
    }

    #[test]
    fn native_transcript_ignores_a_reported_path_without_the_native_shape() {
        let home = TempHome::new("shape");
        let projects = home.0.join(".claude").join("projects");
        let slug = projects.join("-home-user-repo");
        std::fs::create_dir_all(&slug).unwrap();
        std::fs::write(slug.join("id-1.jsonl"), "{}\n").unwrap();
        for reported in [
            home.0.join("id-1.jsonl"),
            projects.join("id-1.jsonl"),
            slug.join("other.jsonl"),
            PathBuf::from("/etc/cron.d/id-1.jsonl"),
        ] {
            // The glob wins over a malformed path...
            assert_eq!(
                native_transcript_locations_in(
                    &claude_session("id-1", Some(reported.clone())),
                    &home.0
                ),
                Some(NativeTranscript {
                    file: slug.join("id-1.jsonl"),
                    side_dir: Some(slug.join("id-1")),
                }),
                "{}",
                reported.display()
            );
            // ...and without a glob hit the malformed path is not returned.
            assert_eq!(
                native_transcript_locations_in(
                    &claude_session("id-9", Some(reported.clone())),
                    &home.0
                ),
                None,
                "{}",
                reported.display()
            );
        }
    }

    #[test]
    fn native_transcript_prefers_the_glob_over_a_stale_reported_path() {
        let home = TempHome::new("stale");
        let projects = home.0.join(".claude").join("projects");
        let old = projects.join("-home-user-old");
        let new = projects.join("-home-user-new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("id-1.jsonl"), "{}\n").unwrap();

        // The reported file moved: the glob finds it and the backup pass
        // is not stuck on NoNativeTranscript forever.
        let stale = claude_session("id-1", Some(old.join("id-1.jsonl")));
        assert_eq!(
            native_transcript_locations_in(&stale, &home.0),
            Some(NativeTranscript {
                file: new.join("id-1.jsonl"),
                side_dir: Some(new.join("id-1")),
            })
        );
        // The reported file exists: it wins even when the glob would find
        // another candidate first.
        std::fs::write(old.join("id-1.jsonl"), "{}\n").unwrap();
        assert_eq!(
            native_transcript_locations_in(&stale, &home.0),
            Some(NativeTranscript {
                file: old.join("id-1.jsonl"),
                side_dir: Some(old.join("id-1")),
            })
        );
        // Gone everywhere: the reported path stays the restore target.
        std::fs::remove_file(old.join("id-1.jsonl")).unwrap();
        std::fs::remove_file(new.join("id-1.jsonl")).unwrap();
        assert_eq!(
            native_transcript_locations_in(&stale, &home.0),
            Some(NativeTranscript {
                file: old.join("id-1.jsonl"),
                side_dir: Some(old.join("id-1")),
            })
        );
    }

    #[test]
    fn native_transcript_falls_back_to_the_claude_projects_glob() {
        let home = TempHome::new("glob");
        let projects = home.0.join(".claude").join("projects");
        let first = projects.join("-home-user-a");
        let second = projects.join("-home-user-b");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("id-2.jsonl"), "{}\n").unwrap();
        std::fs::write(first.join("other.jsonl"), "{}\n").unwrap();

        let session = claude_session("id-2", None);
        assert_eq!(
            native_transcript_locations_in(&session, &home.0),
            Some(NativeTranscript {
                file: second.join("id-2.jsonl"),
                side_dir: Some(second.join("id-2")),
            })
        );
        assert_eq!(
            native_transcript_locations_in(&claude_session("id-3", None), &home.0),
            None
        );
        assert_eq!(
            native_transcript_locations_in(&claude_session("id-2", None), &home.0.join("missing")),
            None
        );
    }

    #[test]
    fn native_transcript_is_unknown_for_other_agents_and_unsafe_ids() {
        let home = TempHome::new("others");
        let codex = PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: AgentSessionRef::id("codex-session").unwrap(),
            transcript_path: Some(home.0.join("codex.jsonl")),
        };
        assert_eq!(native_transcript_locations_in(&codex, &home.0), None);
        let custom = PersistedAgentSession {
            source: "custom:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id("claude-session").unwrap(),
            transcript_path: Some(home.0.join("claude.jsonl")),
        };
        assert_eq!(native_transcript_locations_in(&custom, &home.0), None);
        for id in ["../escape", "a/b", ".hidden", "with space"] {
            let session = claude_session(id, Some(home.0.join("claude.jsonl")));
            assert_eq!(
                native_transcript_locations_in(&session, &home.0),
                None,
                "{id}"
            );
        }
        assert!(is_safe_path_component(
            "0f3c1a2b-4d5e-6f70-8192-a3b4c5d6e7f8"
        ));
        assert!(is_safe_path_component("session_1.v2"));
        assert!(!is_safe_path_component(""));
        assert!(!is_safe_path_component(".."));
    }

    #[test]
    fn graceful_exit_input_is_only_known_for_verified_agents() {
        assert_eq!(graceful_exit_input("claude"), Some("/exit"));
        for agent in ["codex", "pi", "opencode", "unknown"] {
            assert_eq!(graceful_exit_input(agent), None, "{agent}");
        }
    }

    #[test]
    fn native_state_reservation_excludes_full_lifecycle_sources() {
        assert!(is_reserved_native_state_source("herdr:claude", "claude"));
        assert!(is_reserved_native_state_source("herdr:codex", "codex"));
        assert!(is_reserved_native_state_source("herdr:devin", "devin"));
        assert!(!is_reserved_native_state_source("herdr:kimi", "kimi"));
        assert!(!is_reserved_native_state_source(
            "herdr:opencode",
            "opencode"
        ));
    }

    #[test]
    fn codex_noncanonical_resume_launch_has_no_explicit_session() {
        assert_eq!(
            persisted_session_from_launch_args(
                crate::detect::Agent::Codex,
                &["resume".into(), "codex-session".into()]
            )
            .unwrap()
            .session_ref
            .value,
            "codex-session"
        );
        assert!(persisted_session_from_launch_args(
            crate::detect::Agent::Codex,
            &["resume".into(), "--last".into()]
        )
        .is_none());
        assert!(persisted_session_from_launch_args(
            crate::detect::Agent::Codex,
            &["resume".into(), "not-a-session".into(), "--last".into()]
        )
        .is_none());
        assert!(persisted_session_from_launch_args(
            crate::detect::Agent::Codex,
            &[
                "--remote".into(),
                "ws://example.test".into(),
                "resume".into(),
                "remote-session".into(),
            ]
        )
        .is_none());
    }

    #[test]
    fn planner_allows_supported_agents() {
        let pi_session = absolute_test_path("pi-session.jsonl");
        let omp_session = absolute_test_path("omp-session.jsonl");
        assert_eq!(
            plan(
                "herdr:claude",
                "claude",
                &AgentSessionRef::id("claude-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["claude", "--resume", "claude-session"]
        );
        assert_eq!(
            plan(
                "herdr:codex",
                "codex",
                &AgentSessionRef::id("codex-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["codex", "resume", "codex-session"]
        );
        assert_eq!(
            plan(
                "herdr:copilot",
                "copilot",
                &AgentSessionRef::id("copilot-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["copilot", "--resume=copilot-session"]
        );
        assert_eq!(
            plan(
                "herdr:devin",
                "devin",
                &AgentSessionRef::id("devin-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["devin", "--resume", "devin-session"]
        );
        assert_eq!(
            plan(
                "herdr:droid",
                "droid",
                &AgentSessionRef::id("droid-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["droid", "--resume", "droid-session"]
        );
        assert_eq!(
            plan(
                "herdr:kimi",
                "kimi",
                &AgentSessionRef::id("kimi-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["kimi", "--session", "kimi-session"]
        );
        assert_eq!(
            plan(
                "herdr:mastracode",
                "mastracode",
                &AgentSessionRef::id("mastracode-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["mastracode", "--thread", "mastracode-session"]
        );
        assert_eq!(
            plan(
                "herdr:pi",
                "pi",
                &AgentSessionRef::path(&pi_session).unwrap()
            )
            .unwrap()
            .argv,
            vec!["pi", "--session", pi_session.as_str()]
        );
        assert_eq!(
            plan(
                "herdr:omp",
                "omp",
                &AgentSessionRef::path(&omp_session).unwrap()
            )
            .unwrap()
            .argv,
            vec!["omp", format!("--resume={omp_session}").as_str()]
        );
        assert_eq!(
            plan(
                "herdr:hermes",
                "hermes",
                &AgentSessionRef::id("hermes-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["hermes", "--resume", "hermes-session"]
        );
        assert_eq!(
            plan(
                "herdr:opencode",
                "opencode",
                &AgentSessionRef::id("opencode-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["opencode", "--session", "opencode-session"]
        );
        assert_eq!(
            plan(
                "herdr:qodercli",
                "qodercli",
                &AgentSessionRef::id("qoder-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["qodercli", "--resume", "qoder-session"]
        );
        assert_eq!(
            plan(
                "herdr:qwen",
                "qwen",
                &AgentSessionRef::id("qwen-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["qwen", "--resume", "qwen-session"]
        );
        assert_eq!(
            plan(
                "herdr:kilo",
                "kilo",
                &AgentSessionRef::id("kilo-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["kilo", "--session", "kilo-session"]
        );
        assert_eq!(
            plan(
                "herdr:cursor",
                "cursor",
                &AgentSessionRef::id("cursor-session").unwrap()
            )
            .unwrap()
            .argv,
            vec![
                if cfg!(windows) {
                    "cursor-agent.cmd"
                } else {
                    "cursor-agent"
                },
                "--resume",
                "cursor-session",
            ]
        );
        assert_eq!(
            plan(
                "herdr:antigravity_cli",
                "agy",
                &AgentSessionRef::id("agy-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["agy", "--conversation", "agy-session"]
        );
        assert_eq!(
            plan(
                "herdr:grok",
                "grok",
                &AgentSessionRef::id("grok-session").unwrap()
            )
            .unwrap()
            .argv,
            vec!["grok", "--resume", "grok-session"]
        );
        assert_eq!(
            plan(
                "herdr:letta",
                "letta",
                &AgentSessionRef::id("conversation-123").unwrap()
            )
            .unwrap()
            .argv,
            vec!["letta", "--conversation", "conversation-123"]
        );
        assert_eq!(
            plan(
                "herdr:letta",
                "letta",
                &AgentSessionRef::id("default:agent-123").unwrap()
            )
            .unwrap()
            .argv,
            vec!["letta", "--conversation", "default", "--agent", "agent-123"]
        );
        assert!(plan(
            "herdr:letta",
            "letta",
            &AgentSessionRef::id("default:").unwrap()
        )
        .is_none());
    }

    #[test]
    fn planner_rejects_custom_and_unsupported_path_refs() {
        let claude_session = absolute_test_path("claude-session");
        assert!(plan(
            "custom:claude",
            "claude",
            &AgentSessionRef::id("session").unwrap()
        )
        .is_none());
        assert!(plan(
            "herdr:claude",
            "claude",
            &AgentSessionRef::path(&claude_session).unwrap()
        )
        .is_none());
    }

    #[test]
    fn report_ref_prefers_pi_and_omp_paths_and_validates_values() {
        let pi_session = absolute_test_path("pi-session.jsonl");
        let omp_session = absolute_test_path("omp-session.jsonl");
        let claude_session = absolute_test_path("claude-session");
        let copilot_session = absolute_test_path("copilot-session");
        let session_ref = session_ref_from_report(
            "herdr:pi",
            "pi",
            Some("pi-id".into()),
            Some(pi_session.clone()),
        )
        .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Path);
        assert_eq!(session_ref.value, pi_session);

        assert!(session_ref_from_report("herdr:pi", "pi", Some("bad\nid".into()), None).is_none());
        assert!(
            session_ref_from_report("herdr:pi", "pi", None, Some("relative.jsonl".into()))
                .is_none()
        );
        assert!(session_ref_from_report("custom:pi", "pi", Some("pi-id".into()), None).is_none());

        let session_ref = session_ref_from_report(
            "herdr:omp",
            "omp",
            Some("omp-id".into()),
            Some(omp_session.clone()),
        )
        .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Path);
        assert_eq!(session_ref.value, omp_session);

        let session_ref =
            session_ref_from_report("herdr:omp", "omp", Some("omp-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "omp-id");
        let session_ref = session_ref_from_report(
            "herdr:omp",
            "omp",
            Some("omp-id".into()),
            Some("relative.jsonl".into()),
        )
        .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "omp-id");
        assert!(
            session_ref_from_report("herdr:omp", "omp", None, Some("relative.jsonl".into()))
                .is_none()
        );

        assert!(
            session_ref_from_report("herdr:claude", "claude", None, Some(claude_session)).is_none()
        );

        let session_ref =
            session_ref_from_report("herdr:copilot", "copilot", Some("copilot-id".into()), None)
                .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "copilot-id");
        assert!(
            session_ref_from_report("herdr:copilot", "copilot", None, Some(copilot_session))
                .is_none()
        );

        let session_ref =
            session_ref_from_report("herdr:devin", "devin", Some("devin-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "devin-id");

        let session_ref =
            session_ref_from_report("herdr:droid", "droid", Some("droid-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "droid-id");
        assert!(session_ref_from_report(
            "herdr:droid",
            "droid",
            None,
            Some("/tmp/droid-session".into())
        )
        .is_none());

        let session_ref =
            session_ref_from_report("herdr:kimi", "kimi", Some("kimi-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "kimi-id");

        let session_ref = session_ref_from_report(
            "herdr:mastracode",
            "mastracode",
            Some("mastracode-id".into()),
            None,
        )
        .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "mastracode-id");

        let session_ref =
            session_ref_from_report("herdr:kilo", "kilo", Some("kilo-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "kilo-id");

        let session_ref =
            session_ref_from_report("herdr:qodercli", "qodercli", Some("qoder-id".into()), None)
                .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "qoder-id");

        let session_ref =
            session_ref_from_report("herdr:qwen", "qwen", Some("qwen-id".into()), None).unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "qwen-id");

        let session_ref =
            session_ref_from_report("herdr:antigravity_cli", "agy", Some("agy-id".into()), None)
                .unwrap();
        assert_eq!(session_ref.kind, AgentSessionRefKind::Id);
        assert_eq!(session_ref.value, "agy-id");
    }

    #[test]
    fn normalize_session_start_source_allows_known_values() {
        assert_eq!(
            normalize_session_start_source(Some("startup".into())),
            Some("startup".into())
        );
        assert_eq!(
            normalize_session_start_source(Some("resume".into())),
            Some("resume".into())
        );
        assert_eq!(
            normalize_session_start_source(Some("clear".into())),
            Some("clear".into())
        );
        assert_eq!(
            normalize_session_start_source(Some("compact".into())),
            Some("compact".into())
        );
        assert_eq!(
            normalize_session_start_source(Some("branch".into())),
            Some("branch".into())
        );
        assert_eq!(
            normalize_session_start_source(Some("new".into())),
            Some("new".into())
        );
        assert_eq!(
            normalize_session_start_source(Some("fork".into())),
            Some("fork".into())
        );
        assert_eq!(
            normalize_session_start_source(Some("select".into())),
            Some("select".into())
        );
        assert_eq!(
            normalize_session_start_source(Some(" resume ".into())),
            Some("resume".into())
        );
        assert_eq!(normalize_session_start_source(Some("other".into())), None);
        assert_eq!(normalize_session_start_source(None), None);
    }

    #[test]
    fn ids_are_data_not_shell_text() {
        let id = "abc; rm -rf /";
        let codex_plan = plan("herdr:codex", "codex", &AgentSessionRef::id(id).unwrap()).unwrap();
        assert_eq!(codex_plan.argv, vec!["codex", "resume", id]);

        let copilot_plan = plan(
            "herdr:copilot",
            "copilot",
            &AgentSessionRef::id(id).unwrap(),
        )
        .unwrap();
        assert_eq!(copilot_plan.argv, vec!["copilot", "--resume=abc; rm -rf /"]);

        let devin_plan = plan("herdr:devin", "devin", &AgentSessionRef::id(id).unwrap()).unwrap();
        assert_eq!(devin_plan.argv, vec!["devin", "--resume", id]);
    }

    #[test]
    fn planner_rejects_path_refs_for_id_only_agents() {
        let hermes_session = absolute_test_path("hermes-session");
        let opencode_session = absolute_test_path("opencode-session");
        let kilo_session = absolute_test_path("kilo-session");
        let copilot_session = absolute_test_path("copilot-session");
        let devin_session = absolute_test_path("devin-session");
        assert!(plan(
            "herdr:hermes",
            "hermes",
            &AgentSessionRef::path(&hermes_session).unwrap()
        )
        .is_none());
        assert!(plan(
            "herdr:opencode",
            "opencode",
            &AgentSessionRef::path(&opencode_session).unwrap()
        )
        .is_none());
        assert!(plan(
            "herdr:kilo",
            "kilo",
            &AgentSessionRef::path(&kilo_session).unwrap()
        )
        .is_none());
        assert!(plan(
            "herdr:copilot",
            "copilot",
            &AgentSessionRef::path(&copilot_session).unwrap()
        )
        .is_none());
        assert!(plan(
            "herdr:devin",
            "devin",
            &AgentSessionRef::path(&devin_session).unwrap()
        )
        .is_none());
        assert!(session_ref_from_snapshot(
            "herdr:mastracode",
            "mastracode",
            AgentSessionRefKind::Id,
            "mastracode-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "herdr:hermes",
            "hermes",
            AgentSessionRefKind::Id,
            "hermes-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "herdr:opencode",
            "opencode",
            AgentSessionRefKind::Id,
            "opencode-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "herdr:kilo",
            "kilo",
            AgentSessionRefKind::Id,
            "kilo-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "herdr:copilot",
            "copilot",
            AgentSessionRefKind::Id,
            "copilot-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "herdr:devin",
            "devin",
            AgentSessionRefKind::Id,
            "devin-session"
        )
        .is_some());
        assert!(session_ref_from_snapshot(
            "herdr:antigravity_cli",
            "agy",
            AgentSessionRefKind::Id,
            "agy-session"
        )
        .is_some());
        let agy_session = absolute_test_path("agy-session");
        assert!(plan(
            "herdr:antigravity_cli",
            "agy",
            &AgentSessionRef::path(&agy_session).unwrap()
        )
        .is_none());
    }
}
