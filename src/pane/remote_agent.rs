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
    Release,
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
        StatusIdentityDecision::Release => true,
        StatusIdentityDecision::Identify(agent) => {
            if let Some((client, pid)) = probe.remote_client {
                probe.agent = Some(agent);
                probe.agent_pid = Some(pid);
                probe.process_name = Some(client.label().to_string());
                probe.remote = Some(RemoteOrigin {
                    client,
                    via: RemoteVia::ProgramStatus,
                });
            }
            false
        }
    }
}

impl ProgramStatusIdentity {
    /// Per tick: true when the store or the switch changed since the last
    /// tick while a client leads the foreground (or an identity is held),
    /// so the loop probes now instead of at its next scheduled probe.
    pub(super) fn report_changed(&mut self, packed_root: u64, enabled: bool) -> bool {
        let previous = self.last_report.replace((packed_root, enabled));
        previous.is_some_and(|previous| previous != (packed_root, enabled))
            && (self.leader.is_some() || self.held.is_some())
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
        let release_or_keep = if self.held.is_some() {
            StatusIdentityDecision::Release
        } else {
            StatusIdentityDecision::Keep
        };
        if !enabled {
            return release_or_keep;
        }
        // Only a report sent while this client leads the foreground: bytes
        // printed before `ssh host` name nothing on the other side.
        let Some((agent, updated_at)) = root()
            .filter(|report| report.updated_at >= since)
            .and_then(|report| {
                let agent = agent_for_status_app(report.app.as_deref()?)?;
                Some((agent, report.updated_at))
            })
        else {
            return release_or_keep;
        };
        let last_sign = self
            .screen_seen_at
            .filter(|_| self.held == Some(agent))
            .map_or(updated_at, |seen| seen.max(updated_at));
        if now.saturating_duration_since(last_sign) > REMOTE_STATUS_TTL {
            return release_or_keep;
        }
        match self.held {
            Some(held) if held != agent => StatusIdentityDecision::Release,
            _ => StatusIdentityDecision::Identify(agent),
        }
    }
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
        for (enabled, root) in [
            (true, None),
            (true, report("codex", at)),
            (true, report("unknown-tool", at)),
            (false, report("claude-code", at)),
        ] {
            let mut identity = held(Agent::Claude);
            identity.decide(SSH, true, || None, t0);
            assert_eq!(
                identity.decide(SSH, enabled, || root.clone(), at),
                StatusIdentityDecision::Release,
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
            StatusIdentityDecision::Release
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
            StatusIdentityDecision::Release
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
            Instant::now()
        ));
        assert_eq!(probe.agent, Some(Agent::Claude));
        assert_eq!(probe.agent_pid, Some(40));
        assert_eq!(
            probe.remote,
            Some(RemoteOrigin {
                client: RemoteClient::Ssh,
                via: RemoteVia::ProgramStatus,
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
        apply_to_probe(&mut identity, &mut probe, &pane, Instant::now());
        let _ = pane.process_pty_bytes(
            pane_id,
            0,
            b"\x1b]7501;state=working:app=claude-code\x1b\\",
            &tx,
        );
        let mut probe = ssh_probe();
        apply_to_probe(&mut identity, &mut probe, &pane, Instant::now());
        identity.agent_changed(probe.agent, probe.remote);
        assert_eq!(probe.agent, Some(Agent::Claude));

        let _ = pane.process_pty_bytes(pane_id, 0, b"\x1bc", &tx);
        let mut probe = ssh_probe();
        assert!(apply_to_probe(
            &mut identity,
            &mut probe,
            &pane,
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
    fn report_changes_wake_the_probe_only_with_a_client_in_front() {
        let t0 = Instant::now();
        let mut identity = ProgramStatusIdentity::default();
        assert!(!identity.report_changed(1, true), "first tick");
        assert!(!identity.report_changed(2, true), "no client in front");
        identity.decide(SSH, true, || None, t0);
        assert!(!identity.report_changed(2, true), "unchanged");
        assert!(identity.report_changed(3, true));
        assert!(identity.report_changed(3, false), "switch flipped");
    }
}
