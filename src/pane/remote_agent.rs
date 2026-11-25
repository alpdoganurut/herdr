//! Fork: a remote agent identified from its own OSC 7501 status reports.
//!
//! `ssh host`, then `claude` typed in the remote shell, leaves only the ssh
//! client in the pane's foreground with nothing in its argv. Claude Code
//! answers herdr's 7501 probe through ssh and reports `app=claude-code`, so
//! a report that arrived while that client leads the foreground names the
//! agent. Each detector loop keeps one [`ProgramStatusIdentity`] and asks it
//! after every process probe that found no agent.
//!
//! Pane output is untrusted: anything on the other side can print the
//! report. So the identity only lets the screen manifests run and marks the
//! agent remote ([`RemoteVia::ProgramStatus`]); typed messages still wait
//! for the screen to read idle, and every feature that would act on the
//! local process (the ssh client) or needs a herdr hook is refused for it.
//!
//! The identity ends, with a process-exit report so waiters see it finish:
//! - when the root record is cleared (`state=clear`, which Claude Code sends
//!   on exit; RIS; a new probe), or the switch is turned off;
//! - when the root names another app, or one no manifest knows;
//! - when it goes stale ([`REMOTE_STATUS_TTL`]);
//! - when the client leaves the foreground (the usual process path).

use std::time::{Duration, Instant};

use crate::detect::remote::{RemoteClient, RemoteOrigin, RemoteVia};
use crate::detect::Agent;

/// How long a reported identity lasts with neither a new report nor its
/// own screen. The protocol has no heartbeat: an idle Claude Code reports
/// nothing for hours, so the report's age alone says nothing about whether
/// it still runs. While it runs, its screen keeps reading as its own UI
/// (prompt box, spinner, dialog: a manifest rule with `visible_idle`,
/// `visible_working` or `visible_blocker`); one that died without its
/// `state=clear` (killed, crashed) leaves a remote shell prompt that matches
/// none. Ten minutes covers its screens that match no such rule (the
/// transcript viewer, pickers) without keeping a dead agent for long.
pub(super) const REMOTE_STATUS_TTL: Duration = Duration::from_secs(10 * 60);

/// The root record as the identity reads it (one core lock per probe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RootReport {
    /// The app the root reported, as sent (`claude-code`).
    pub app: Option<String>,
    pub updated_at: Instant,
}

/// What the probe should report for a client in front with no agent found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusIdentityDecision {
    /// Nothing to add: the probe stands as it is.
    Keep,
    /// The agent the reports name runs behind the client.
    Identify(Agent),
    /// The held identity ends now (not after the usual missed probes).
    Release(ReleaseReason),
}

/// Why a held identity ended (logged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReleaseReason {
    /// The root record is gone: `state=clear`, RIS or a new probe (the
    /// store cannot tell them apart).
    Cleared,
    /// The root names another app, or one no manifest knows.
    OtherApp,
    /// `[agents] program_status = false`.
    SwitchOff,
    /// Neither a report nor its own screen for [`REMOTE_STATUS_TTL`].
    Stale,
    /// Another client leads the foreground and has sent no report yet.
    NewClient,
}

impl ReleaseReason {
    fn label(self) -> &'static str {
        match self {
            Self::Cleared => "record cleared (state=clear, RIS or new probe)",
            Self::OtherApp => "another app reported",
            Self::SwitchOff => "program_status switched off",
            Self::Stale => "no report or own screen within the TTL",
            Self::NewClient => "a new client leads the foreground",
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct ProgramStatusIdentity {
    /// The client process that leads the foreground, and since when.
    leader: Option<(u32, Instant)>,
    /// The agent this loop holds through reports.
    held: Option<Agent>,
    /// The last tick the held agent's screen read as its own UI.
    screen_seen_at: Option<Instant>,
    /// The packed root and switch as of the last tick.
    last_report: Option<(u64, bool)>,
}

/// The agent an OSC 7501 `app` names, through the manifest labels and
/// aliases (`claude-code` → claude).
pub(super) fn agent_for_status_app(app: &str) -> Option<Agent> {
    if app.is_empty() || app.contains(['/', '\\']) {
        return None;
    }
    crate::detect::parse_agent_label(app)
}

/// Fork: runs [`ProgramStatusIdentity::decide`] for a probe that found no
/// agent and applies an identification to it. Returns true when the held
/// identity is released (the loop reports the agent's exit now).
pub(super) fn apply_to_probe(
    identity: &mut ProgramStatusIdentity,
    probe: &mut super::ProcessProbeResult,
    terminal: &super::PaneTerminal,
    pane_id: crate::layout::PaneId,
    now: Instant,
) -> bool {
    if probe.agent.is_some() {
        return false;
    }
    match identity.decide(
        probe.remote_client,
        super::program_status::enabled(),
        || terminal.program_status_root_report(),
        now,
    ) {
        StatusIdentityDecision::Keep => false,
        StatusIdentityDecision::Release(reason) => {
            tracing::info!(
                pane = pane_id.raw(),
                agent = ?identity.held,
                client = ?probe.remote_client.map(|(client, _)| client.label()),
                via = RemoteVia::ProgramStatus.label(),
                reason = reason.label(),
                "remote agent identity released"
            );
            true
        }
        StatusIdentityDecision::Identify(agent) => {
            if let Some((client, pid)) = probe.remote_client {
                if identity.held != Some(agent) {
                    tracing::info!(
                        pane = pane_id.raw(),
                        ?agent,
                        client = client.label(),
                        client_pid = pid,
                        via = RemoteVia::ProgramStatus.label(),
                        "remote agent identified from its status reports"
                    );
                }
                probe.agent = Some(agent);
                probe.agent_pid = Some(pid);
                probe.process_name = Some(client.label().to_string());
                probe.remote = Some(RemoteOrigin {
                    client,
                    via: RemoteVia::ProgramStatus,
                    client_process: None,
                });
            }
            false
        }
    }
}

impl ProgramStatusIdentity {
    /// Per tick: true when the store or the switch changed since the last
    /// tick while the client this loop saw lead the foreground still leads
    /// it (`foreground_pgid`), so the loop probes now instead of at its next
    /// scheduled probe. Reports with any other job in front wake nothing:
    /// the usual probe schedule (and the lifecycle-hook skip) stands.
    pub(super) fn report_changed(
        &mut self,
        packed_root: u64,
        enabled: bool,
        foreground_pgid: Option<u32>,
    ) -> bool {
        let previous = self.last_report.replace((packed_root, enabled));
        previous.is_some_and(|previous| previous != (packed_root, enabled))
            && foreground_pgid.is_some()
            && self.leader.map(|(pid, _)| pid) == foreground_pgid
    }

    /// A detection reset (`detect_reset`): forget the held identity and the
    /// reports seen, but keep the client leading the foreground and since
    /// when, so a report it already carried (an idle remote Claude reports
    /// nothing more) still names the agent at the next probe.
    pub(super) fn reset_detection(&mut self) {
        self.held = None;
        self.screen_seen_at = None;
        self.last_report = None;
    }

    /// Per tick: the held agent's screen still reads as its own UI.
    pub(super) fn note_screen(&mut self, agent: Option<Agent>, own_screen: bool, now: Instant) {
        if own_screen && agent.is_some() && agent == self.held {
            self.screen_seen_at = Some(now);
        }
    }

    /// The loop's agent changed (`remote` is the identification's origin).
    pub(super) fn agent_changed(&mut self, agent: Option<Agent>, remote: Option<RemoteOrigin>) {
        self.held =
            agent.filter(|_| remote.is_some_and(|remote| remote.via == RemoteVia::ProgramStatus));
        self.screen_seen_at = None;
    }

    /// For a probe that found no agent: `client` is the remote-shell client
    /// leading the foreground (`None` when it is not one, or is the pane's
    /// own process), `root` reads the root record.
    pub(super) fn decide(
        &mut self,
        client: Option<(RemoteClient, u32)>,
        enabled: bool,
        root: impl FnOnce() -> Option<RootReport>,
        now: Instant,
    ) -> StatusIdentityDecision {
        let Some((_, leader_pid)) = client else {
            // The process path reports the client's exit (or holds it in
            // the background, like any agent). The leader is kept: the same
            // client back in front (`fg`, a probe that could not read the
            // job) keeps its reports; only another client starts over.
            return StatusIdentityDecision::Keep;
        };
        let since = match self.leader {
            Some((pid, since)) if pid == leader_pid => since,
            _ => {
                self.leader = Some((leader_pid, now));
                now
            }
        };
        let held = self.held.is_some();
        let release_or_keep = |reason| {
            if held {
                StatusIdentityDecision::Release(reason)
            } else {
                StatusIdentityDecision::Keep
            }
        };
        if !enabled {
            return release_or_keep(ReleaseReason::SwitchOff);
        }
        let Some(report) = root() else {
            return release_or_keep(ReleaseReason::Cleared);
        };
        // Only a report sent while this client leads the foreground: bytes
        // printed before `ssh host` name nothing on the other side.
        if report.updated_at < since {
            return release_or_keep(ReleaseReason::NewClient);
        }
        let Some(agent) = report.app.as_deref().and_then(agent_for_status_app) else {
            return release_or_keep(ReleaseReason::OtherApp);
        };
        let updated_at = report.updated_at;
        let last_sign = self
            .screen_seen_at
            .filter(|_| self.held == Some(agent))
            .map_or(updated_at, |seen| seen.max(updated_at));
        if now.saturating_duration_since(last_sign) > REMOTE_STATUS_TTL {
            return release_or_keep(ReleaseReason::Stale);
        }
        match self.held {
            Some(held) if held != agent => StatusIdentityDecision::Release(ReleaseReason::OtherApp),
            _ => StatusIdentityDecision::Identify(agent),
        }
    }
}

/// Fork: keeps the pane's remote mark (`TerminalState::agent_remote`,
/// runtime only) in step with what the loop's probes find. The mark is
/// first sent with the detection of a new agent; a pane restored or handed
/// off with its agent already known never detects it anew, so a probe that
/// finds that same agent behind a client must send the mark on its own,
/// or every guard that refuses to act on the client process fails open.
#[derive(Debug, Default)]
pub(super) struct RemoteMarkSync {
    /// The mark last sent: agent, client, how, and the client's pid.
    sent: Option<(Agent, RemoteClient, RemoteVia, Option<u32>)>,
}

impl RemoteMarkSync {
    fn key(
        agent: Agent,
        remote: Option<RemoteOrigin>,
        client_pid: Option<u32>,
    ) -> Option<(Agent, RemoteClient, RemoteVia, Option<u32>)> {
        remote.map(|remote| (agent, remote.client, remote.via, client_pid))
    }

    /// The loop published the detection of `agent` with `remote`.
    pub(super) fn detected(
        &mut self,
        agent: Agent,
        remote: Option<RemoteOrigin>,
        client_pid: Option<u32>,
    ) {
        self.sent = Self::key(agent, remote, client_pid);
    }

    /// A detection reset: nothing is known to be sent.
    pub(super) fn reset(&mut self) {
        self.sent = None;
    }

    /// After a probe that published no detection: the mark to send (with
    /// its client process pinned) when the probe found the loop's current
    /// agent behind a client other than the mark last sent names, or found
    /// it local while a remote mark was sent. `Some(None)` clears the mark.
    pub(super) fn remark(
        &mut self,
        current: Option<Agent>,
        probe_agent: Option<Agent>,
        probe_remote: Option<RemoteOrigin>,
        client_pid: Option<u32>,
    ) -> Option<(Agent, Option<RemoteOrigin>)> {
        let Some(agent) = current else {
            self.sent = None;
            return None;
        };
        if probe_agent != Some(agent) {
            return None;
        }
        let key = Self::key(agent, probe_remote, client_pid);
        if key == self.sent {
            return None;
        }
        self.sent = key;
        Some((agent, pin_client_process(probe_remote, client_pid)))
    }
}

/// The origin to publish: pinned to the client process that stands in as
/// the agent's process.
pub(super) fn pin_client_process(
    remote: Option<RemoteOrigin>,
    client_pid: Option<u32>,
) -> Option<RemoteOrigin> {
    remote.map(|remote| match client_pid {
        Some(pid) => remote.with_client_process(pid),
        None => remote,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SSH: Option<(RemoteClient, u32)> = Some((RemoteClient::Ssh, 40));

    fn report(app: &str, updated_at: Instant) -> Option<RootReport> {
        Some(RootReport {
            app: Some(app.to_string()),
            updated_at,
        })
    }

    fn held(agent: Agent) -> ProgramStatusIdentity {
        let mut identity = ProgramStatusIdentity::default();
        identity.agent_changed(
            Some(agent),
            Some(RemoteOrigin {
                client: RemoteClient::Ssh,
                via: RemoteVia::ProgramStatus,
                client_process: None,
            }),
        );
        identity
    }

    #[test]
    fn a_report_sent_while_ssh_leads_names_the_agent() {
        let t0 = Instant::now();
        let mut identity = ProgramStatusIdentity::default();
        // ssh is seen first, Claude starts in the remote shell later.
        assert_eq!(
            identity.decide(SSH, true, || None, t0),
            StatusIdentityDecision::Keep
        );
        let at = t0 + Duration::from_secs(5);
        assert_eq!(
            identity.decide(SSH, true, || report("claude-code", at), at),
            StatusIdentityDecision::Identify(Agent::Claude)
        );
    }

    #[test]
    fn no_client_unknown_apps_old_reports_and_the_off_switch_name_nothing() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(5);
        let mut identity = ProgramStatusIdentity::default();
        assert_eq!(
            identity.decide(None, true, || report("claude-code", t0), t0),
            StatusIdentityDecision::Keep
        );
        for app in ["vim", "claude/code", ""] {
            assert_eq!(
                identity.decide(SSH, true, || report(app, later), later),
                StatusIdentityDecision::Keep,
                "{app}"
            );
        }
        // Printed before this ssh led the foreground.
        let mut identity = ProgramStatusIdentity::default();
        assert_eq!(
            identity.decide(SSH, true, || report("claude-code", t0), later),
            StatusIdentityDecision::Keep
        );
        let mut identity = ProgramStatusIdentity::default();
        identity.decide(SSH, true, || None, t0);
        assert_eq!(
            identity.decide(SSH, false, || report("claude-code", later), later),
            StatusIdentityDecision::Keep
        );
        // A record no newer than the TTL never starts an identity.
        let mut identity = ProgramStatusIdentity::default();
        identity.decide(SSH, true, || None, t0);
        let stale = later + REMOTE_STATUS_TTL + Duration::from_secs(1);
        assert_eq!(
            identity.decide(SSH, true, || report("claude-code", later), stale),
            StatusIdentityDecision::Keep
        );
    }

    #[test]
    fn a_held_identity_is_released_on_clear_another_app_or_the_switch() {
        let t0 = Instant::now();
        let at = t0 + Duration::from_secs(1);
        for (enabled, root, reason) in [
            (true, None, ReleaseReason::Cleared),
            (true, report("codex", at), ReleaseReason::OtherApp),
            (true, report("unknown-tool", at), ReleaseReason::OtherApp),
            (false, report("claude-code", at), ReleaseReason::SwitchOff),
        ] {
            let mut identity = held(Agent::Claude);
            identity.decide(SSH, true, || None, t0);
            assert_eq!(
                identity.decide(SSH, enabled, || root.clone(), at),
                StatusIdentityDecision::Release(reason),
                "{root:?} enabled={enabled}"
            );
        }
    }

    #[test]
    fn an_idle_agent_whose_screen_is_still_its_own_stays_past_the_ttl() {
        let t0 = Instant::now();
        let mut identity = held(Agent::Claude);
        identity.decide(SSH, true, || None, t0);
        let reported = t0 + Duration::from_secs(1);
        let much_later = reported + REMOTE_STATUS_TTL * 6;
        // No report for an hour, but its prompt box is still on screen.
        identity.note_screen(Some(Agent::Claude), true, much_later);
        assert_eq!(
            identity.decide(
                SSH,
                true,
                || report("claude-code", reported),
                much_later + Duration::from_secs(5)
            ),
            StatusIdentityDecision::Identify(Agent::Claude)
        );
    }

    #[test]
    fn a_held_identity_with_neither_reports_nor_its_screen_goes_stale() {
        let t0 = Instant::now();
        let mut identity = held(Agent::Claude);
        identity.decide(SSH, true, || None, t0);
        let reported = t0 + Duration::from_secs(1);
        identity.note_screen(Some(Agent::Claude), true, reported);
        // The remote shell prompt is back: nothing on screen is Claude's.
        identity.note_screen(Some(Agent::Claude), false, reported + REMOTE_STATUS_TTL);
        assert_eq!(
            identity.decide(
                SSH,
                true,
                || report("claude-code", reported),
                reported + REMOTE_STATUS_TTL + Duration::from_secs(1)
            ),
            StatusIdentityDecision::Release(ReleaseReason::Stale)
        );
    }

    #[test]
    fn a_new_client_needs_its_own_report() {
        let t0 = Instant::now();
        let mut identity = held(Agent::Claude);
        identity.decide(SSH, true, || None, t0);
        let reported = t0 + Duration::from_secs(1);
        assert_eq!(
            identity.decide(SSH, true, || report("claude-code", reported), reported),
            StatusIdentityDecision::Identify(Agent::Claude)
        );
        let other = Some((RemoteClient::Ssh, 41));
        assert_eq!(
            identity.decide(
                other,
                true,
                || report("claude-code", reported),
                reported + Duration::from_secs(1)
            ),
            StatusIdentityDecision::Release(ReleaseReason::NewClient)
        );
    }

    fn ssh_probe() -> super::super::ProcessProbeResult {
        super::super::ProcessProbeResult {
            process_group_id: Some(40),
            foreground_is_pane_shell: false,
            agent: None,
            agent_pid: None,
            process_name: None,
            remote: None,
            remote_client: SSH,
        }
    }

    #[test]
    fn fork_smoke_a_claude_report_through_ssh_identifies_the_probe_and_its_clear_releases_it() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = super::super::PaneTerminal::new(
            super::super::GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
        );
        let pane_id = crate::layout::PaneId::from_raw(1);
        let mut identity = ProgramStatusIdentity::default();

        // `ssh host`: a remote shell, nothing reported yet.
        let mut probe = ssh_probe();
        assert!(!apply_to_probe(
            &mut identity,
            &mut probe,
            &pane,
            pane_id,
            Instant::now()
        ));
        assert_eq!(probe.agent, None);

        // `claude` starts on the other side and reports through ssh.
        let _ = pane.process_pty_bytes(
            pane_id,
            0,
            b"\x1b]7501;state=idle:app=claude-code\x1b\\",
            &tx,
        );
        let mut probe = ssh_probe();
        assert!(!apply_to_probe(
            &mut identity,
            &mut probe,
            &pane,
            pane_id,
            Instant::now()
        ));
        assert_eq!(probe.agent, Some(Agent::Claude));
        assert_eq!(probe.agent_pid, Some(40));
        assert_eq!(
            probe.remote,
            Some(RemoteOrigin {
                client: RemoteClient::Ssh,
                via: RemoteVia::ProgramStatus,
                client_process: None,
            })
        );
        identity.agent_changed(probe.agent, probe.remote);

        // `/exit`: Claude Code clears its record; ssh stays in front.
        let _ = pane.process_pty_bytes(pane_id, 0, b"\x1b]7501;state=clear\x1b\\", &tx);
        let mut probe = ssh_probe();
        assert!(apply_to_probe(
            &mut identity,
            &mut probe,
            &pane,
            pane_id,
            Instant::now()
        ));
        assert_eq!(probe.agent, None);
    }

    #[test]
    fn ris_releases_a_held_identity() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = super::super::PaneTerminal::new(
            super::super::GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
        );
        let pane_id = crate::layout::PaneId::from_raw(1);
        let mut identity = ProgramStatusIdentity::default();
        let mut probe = ssh_probe();
        apply_to_probe(&mut identity, &mut probe, &pane, pane_id, Instant::now());
        let _ = pane.process_pty_bytes(
            pane_id,
            0,
            b"\x1b]7501;state=working:app=claude-code\x1b\\",
            &tx,
        );
        let mut probe = ssh_probe();
        apply_to_probe(&mut identity, &mut probe, &pane, pane_id, Instant::now());
        identity.agent_changed(probe.agent, probe.remote);
        assert_eq!(probe.agent, Some(Agent::Claude));

        let _ = pane.process_pty_bytes(pane_id, 0, b"\x1bc", &tx);
        let mut probe = ssh_probe();
        assert!(apply_to_probe(
            &mut identity,
            &mut probe,
            &pane,
            pane_id,
            Instant::now()
        ));
    }

    #[test]
    fn the_same_client_back_in_front_keeps_its_identity() {
        let t0 = Instant::now();
        let mut identity = held(Agent::Claude);
        identity.decide(SSH, true, || None, t0);
        let reported = t0 + Duration::from_secs(1);
        // ssh suspended (`~^Z`): the shell is in front for a while.
        assert_eq!(
            identity.decide(None, true, || report("claude-code", reported), reported),
            StatusIdentityDecision::Keep
        );
        // `fg`: the same ssh, no new report.
        assert_eq!(
            identity.decide(
                SSH,
                true,
                || report("claude-code", reported),
                reported + Duration::from_secs(30)
            ),
            StatusIdentityDecision::Identify(Agent::Claude)
        );
    }

    #[test]
    fn report_changes_wake_the_probe_only_while_the_seen_client_leads() {
        let t0 = Instant::now();
        let mut identity = ProgramStatusIdentity::default();
        assert!(!identity.report_changed(1, true, Some(40)), "first tick");
        assert!(
            !identity.report_changed(2, true, Some(40)),
            "no client seen yet"
        );
        identity.decide(SSH, true, || None, t0);
        assert!(!identity.report_changed(2, true, Some(40)), "unchanged");
        assert!(identity.report_changed(3, true, Some(40)));
        assert!(
            identity.report_changed(3, false, Some(40)),
            "switch flipped"
        );
        // ssh has exited (the leader is kept for `fg`): a local agent's
        // reports in front of the shell no longer force a probe each tick.
        assert!(!identity.report_changed(4, false, Some(77)), "other job");
        assert!(!identity.report_changed(5, false, None), "no job");
        // Held, but its client is in the background.
        let mut identity = held(Agent::Claude);
        identity.decide(SSH, true, || None, t0);
        identity.report_changed(1, true, Some(10));
        assert!(
            !identity.report_changed(2, true, Some(10)),
            "held, shell in front"
        );
        assert!(
            identity.report_changed(3, true, Some(40)),
            "held, back in front"
        );
    }

    #[test]
    fn a_detection_reset_keeps_the_client_and_its_earlier_report() {
        let t0 = Instant::now();
        let mut identity = ProgramStatusIdentity::default();
        identity.decide(SSH, true, || None, t0);
        // An idle remote Claude reported once, then a reset (e.g. an agent
        // relaunch request) arrives before the next probe.
        let reported = t0 + Duration::from_secs(1);
        identity.reset_detection();
        assert_eq!(
            identity.decide(
                SSH,
                true,
                || report("claude-code", reported),
                reported + Duration::from_secs(2)
            ),
            StatusIdentityDecision::Identify(Agent::Claude)
        );
    }
}
