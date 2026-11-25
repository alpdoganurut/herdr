//! The remote browser in the hub (fork): the companion profile while the
//! user's Mac serves it over the link ([`crate::browser::link`]).
//!
//! A child module of `hub` (it reaches the hub's private state). The link
//! has its own pending calls and ids; `inner.host` (the local sidecar) and
//! `inner.pending` are never touched here, and a local sidecar that goes
//! touches nothing of it. The lease of `companion_host` is the one thing
//! both share: a linked browser holds it like a registered one, renewed by
//! the helper's pings instead of heartbeats.
//!
//! Lock order: `companion_host`, then the link slot.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::companion_host::{
    CompanionLease, LinkLease, ShowRequest, COMPANION_PROFILE, REPORT_RATE_PER_S,
};
use super::{is_companion, BrowserHub, HostEvent, HostReply, TabKey, ACTIVITY, REPLY_GRACE};
use crate::api::schema::BrowserRunResult;
use crate::browser::link::{self, CallError, Link, LinkEnd, LinkHandler, LinkSlot, OpenFacts};
use crate::browser::link_proto::{
    bye, code, op_rule, text, BlobName, BrowserLinkOpenParams, BrowserLinkReady, EnsureReply,
    FileReply, FileRequest, HostUp, OpGate, RefuseRule, ServerFrame, ENSURE_MS, RELEASE_MAX_KEYS,
    REPLY_EXTRA_MS,
};
use crate::browser::state::ProfileStatus;
use crate::browser::{unix_now, BrowserError};

/// Where an op on the companion profile goes.
#[derive(Debug, Clone)]
pub(crate) enum LinkRoute {
    /// The Mac serves it.
    Up(Arc<Link>),
    /// The link is there, its browser is not (yet).
    Down(Arc<Link>),
    /// The link dropped, its lease still lives.
    Reconnecting { label: String },
    /// No link: this machine's companion, if any.
    Local,
}

/// A screenshot's `path` and `inline_path`, taken out of its arguments.
fn take_paths(args: &mut Value) -> Vec<(BlobName, std::path::PathBuf)> {
    let Some(fields) = args.as_object_mut() else {
        return Vec::new();
    };
    [("path", BlobName::Full), ("inline_path", BlobName::Inline)]
        .into_iter()
        .filter_map(|(key, name)| {
            let path = fields.remove(key)?;
            Some((name, std::path::PathBuf::from(path.as_str()?)))
        })
        .collect()
}

/// The files a screenshot's pictures went to, in its result as a local
/// sidecar names them; without the full picture there is none.
fn put_paths(
    result: &mut Value,
    sinks: &[(BlobName, std::path::PathBuf)],
    written: &[BlobName],
    label: &str,
) -> Result<(), BrowserError> {
    if !result.is_object() {
        *result = json!({});
    }
    let path = |name: BlobName| {
        sinks
            .iter()
            .find(|(sink, _)| *sink == name && written.contains(&name))
            .map(|(_, path)| path.display().to_string())
    };
    let Some(full) = path(BlobName::Full) else {
        // the small one alone is no screenshot
        for name in written {
            if let Some((_, dest)) = sinks.iter().find(|(sink, _)| sink == name) {
                let _ = std::fs::remove_file(dest);
            }
        }
        return Err(BrowserError::host_failed(format!(
            "the screenshot did not arrive from {label}; nothing was saved"
        )));
    };
    if let Some(fields) = result.as_object_mut() {
        fields.remove("full");
        fields.remove("inline");
        fields.insert("path".into(), json!(full));
        fields.insert("inline_path".into(), json!(path(BlobName::Inline)));
    }
    Ok(())
}

/// The linked browser closed between two steps of one call.
fn down(link: &Link) -> BrowserError {
    unavailable(text::start_failed(&link.label, "it just closed"))
}

fn unavailable(message: String) -> BrowserError {
    BrowserError::new(code::HOST_UNAVAILABLE, message)
}

impl BrowserHub {
    /// The server's link slot.
    pub(crate) fn link_slot(&self) -> &LinkSlot {
        &self.inner.link
    }

    /// `browser.link_open`: refuse it, or make it the link. Decided with the
    /// companion's lease held: a registration and a link never both win.
    pub(crate) fn link_open(
        &self,
        params: &BrowserLinkOpenParams,
        mut facts: OpenFacts,
    ) -> Result<Arc<Link>, BrowserError> {
        let config = self.config();
        facts.enabled = config.enabled;
        facts.off = config.link_off();
        // a companion this server runs registers again within its heartbeat
        facts.local_host |= self.launcher().lock().busy();
        let lease = self.inner.companion_host.lock().unwrap();
        let now = Instant::now();
        match lease.as_ref().filter(|lease| lease.live(now)) {
            Some(CompanionLease {
                link: Some(held), ..
            }) if held.host_id != params.host_id.trim() => {
                facts.held = Some((held.label.clone(), held.since.clone()));
            }
            Some(CompanionLease { link: None, .. }) => facts.local_host = true,
            _ => {}
        }
        self.inner.link.open(params, &facts)
    }

    /// Where an op on the companion profile goes now.
    pub(crate) fn link_route(&self) -> LinkRoute {
        let now = Instant::now();
        let lease = self.inner.companion_host.lock().unwrap();
        let lease = lease.as_ref().filter(|lease| lease.live(now));
        let held = lease.and_then(|lease| lease.link.as_ref());
        match (held, self.inner.link.live()) {
            (Some(held), Some(link)) if held.epoch == link.epoch => LinkRoute::Up(link),
            (Some(held), Some(link)) if held.host_id == link.host_id => LinkRoute::Down(link),
            (Some(held), _) => LinkRoute::Reconnecting {
                label: held.label.clone(),
            },
            (None, Some(link)) if lease.is_none() => LinkRoute::Down(link),
            (None, _) => LinkRoute::Local,
        }
    }

    /// The Mac the browser is on, while it is linked or reconnecting.
    pub(crate) fn linked_to(&self) -> Option<String> {
        match self.link_route() {
            LinkRoute::Up(link) | LinkRoute::Down(link) => Some(link.label.clone()),
            LinkRoute::Reconnecting { label } => Some(label),
            LinkRoute::Local => None,
        }
    }

    /// [`Self::linked_to`] for a caller that holds the lease.
    pub(super) fn linked_label(
        &self,
        lease: Option<&CompanionLease>,
        now: Instant,
    ) -> Option<String> {
        let held = lease
            .filter(|lease| lease.live(now))
            .and_then(|lease| lease.link.as_ref())
            .map(|held| held.label.clone());
        held.or_else(|| self.inner.link.live().map(|link| link.label.clone()))
    }

    /// The link while its browser serves ops.
    pub(crate) fn link_up(&self) -> Option<Arc<Link>> {
        match self.link_route() {
            LinkRoute::Up(link) => Some(link),
            _ => None,
        }
    }

    /// Whether the linked browser serves ops, or why not; `None`: no link.
    pub(crate) fn link_attached(&self) -> Option<Result<(), BrowserError>> {
        match self.link_route() {
            LinkRoute::Up(_) => Some(Ok(())),
            LinkRoute::Down(link) => Some(Err(down(&link))),
            LinkRoute::Reconnecting { label } => Some(Err(unavailable(text::reconnecting(&label)))),
            LinkRoute::Local => None,
        }
    }

    /// The controls of this machine's companion: none while a Mac has the
    /// browser.
    pub(crate) fn companion_not_linked(&self) -> Result<(), BrowserError> {
        match self.linked_to() {
            Some(label) => Err(BrowserError::new(
                code::HOST_IN_USE,
                text::control_there(&label),
            )),
            None => Ok(()),
        }
    }

    /// A companion on this machine holds a live lease.
    pub(crate) fn local_companion_live(&self) -> bool {
        self.companion_live() && !self.lease_is_linked()
    }

    /// The lease is a linked browser's.
    pub(crate) fn lease_is_linked(&self) -> bool {
        self.inner
            .companion_host
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|lease| lease.link.is_some())
    }

    /// Make sure the linked browser serves ops; false: there is no link. On
    /// the caller's connection thread, at most [`ENSURE_MS`], with no lock
    /// held.
    pub(crate) fn ensure_link(&self, reason: &str) -> Result<bool, BrowserError> {
        match self.link_route() {
            LinkRoute::Up(_) => Ok(true),
            LinkRoute::Local => Ok(false),
            LinkRoute::Reconnecting { label } => Err(unavailable(text::reconnecting(&label))),
            LinkRoute::Down(link) => self.ensure_over(&link, reason).map(|()| true),
        }
    }

    fn ensure_over(&self, link: &Arc<Link>, reason: &str) -> Result<(), BrowserError> {
        let label = &link.label;
        match link.ready {
            BrowserLinkReady::Ready => {}
            BrowserLinkReady::NotInstalled => return Err(unavailable(text::not_installed(label))),
            BrowserLinkReady::Failed | BrowserLinkReady::Unknown => {
                return Err(unavailable(text::start_failed(label, &link.detail)))
            }
        }
        let until = Instant::now() + Duration::from_millis(ENSURE_MS);
        match link.ensure(reason, Duration::from_millis(ENSURE_MS)) {
            Ok(EnsureReply { ok: true, .. }) => {}
            Ok(EnsureReply { error, .. }) => {
                let why = error
                    .map(|error| link::clip_text(&error.message))
                    .unwrap_or_default();
                return Err(unavailable(text::start_failed(label, &why)));
            }
            Err(CallError::Timeout) => {
                return Err(unavailable(text::start_failed(
                    label,
                    &format!("no answer within {} s", ENSURE_MS / 1000),
                )))
            }
            Err(_) => return Err(unavailable(text::reconnecting(label))),
        }
        // its `host_up` comes before the answer; a late one is waited for
        let until = until.min(Instant::now() + Duration::from_secs(5));
        loop {
            match self.link_route() {
                LinkRoute::Up(_) => return Ok(()),
                LinkRoute::Down(now) if now.epoch == link.epoch && Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                LinkRoute::Down(now) if now.epoch == link.epoch => {
                    return Err(unavailable(text::start_failed(label, "")))
                }
                _ => return Err(unavailable(text::reconnecting(label))),
            }
        }
    }

    /// The companion profile's op, over the link when it is there; `None`:
    /// the local sidecar's.
    pub(crate) fn request_over_link(
        &self,
        op: &str,
        target: Option<&str>,
        args: &Value,
        deadline: Duration,
    ) -> Option<Result<HostReply, BrowserError>> {
        match self.link_route() {
            LinkRoute::Up(link) => Some(self.link_call(&link, op, target, args.clone(), deadline)),
            LinkRoute::Down(link) => Some(Err(down(&link))),
            LinkRoute::Reconnecting { label } => Some(Err(unavailable(text::reconnecting(&label)))),
            LinkRoute::Local => None,
        }
    }

    /// One op over the link. Only what the op table lets through is sent: no
    /// `dashboard`, `ntp`, `show`, `watch`, `unwatch`, `attach`, `detach`.
    pub(crate) fn link_call(
        &self,
        link: &Arc<Link>,
        op: &str,
        target: Option<&str>,
        args: Value,
        deadline: Duration,
    ) -> Result<HostReply, BrowserError> {
        let label = &link.label;
        if !op_rule(op).is_some_and(|rule| rule.gate == OpGate::Allowed) {
            return Err(BrowserError::new(
                code::HOST_REFUSED,
                text::refused(label, op, RefuseRule::LocalOp),
            ));
        }
        let activity = ACTIVITY.with(|slot| slot.borrow().clone());
        let wait = deadline + REPLY_GRACE + Duration::from_millis(REPLY_EXTRA_MS);
        // a screenshot's pictures come in pieces and go where the hub says;
        // no path of this machine goes to the Mac
        let mut args = args;
        let sinks = if op == "screenshot" {
            take_paths(&mut args)
        } else {
            Vec::new()
        };
        match link.call(op, target, args, activity, deadline, wait, &sinks) {
            Ok((reply, written)) if reply.ok => {
                let mut reply: HostReply = reply.into();
                if let Some(page) = reply.page.as_mut() {
                    if let Some(url) = self.inner.link.files().to_file_url(&page.url) {
                        page.url = url;
                    }
                }
                if !sinks.is_empty() {
                    put_paths(&mut reply.result, &sinks, &written, label)?;
                }
                Ok(reply)
            }
            Ok((reply, _)) => {
                let error = reply.error.unwrap_or_default();
                let code = if error.code.is_empty() {
                    "browser_error"
                } else {
                    &error.code
                };
                Err(BrowserError::new(code, error.message).with_target(error.target))
            }
            Err(CallError::NotSent) => Err(unavailable(text::reconnecting(label))),
            Err(CallError::Busy) => Err(BrowserError::new(code::HOST_BUSY, text::busy())),
            Err(CallError::TooLong) => Err(BrowserError::new(
                code::INVALID_REQUEST,
                format!("{op} is too large for the connection to {label}; nothing was sent"),
            )),
            Err(CallError::Lost) => Err(BrowserError::new(
                code::HOST_LINK_LOST,
                text::link_lost(label, op),
            )),
            Err(CallError::Timeout) => Err(BrowserError::timeout(format!(
                "{op} exceeded {} ms (the page may still be loading; try `herdr browser read`)",
                deadline.as_millis()
            ))),
        }
    }

    /// `show` on a linked browser: the file's directory is granted to the
    /// Mac for this epoch and the page loads from it once (no watch, no
    /// network, no page API: the notes say so).
    #[allow(clippy::too_many_arguments)] // execute_show's own
    pub(super) fn execute_show_remote(
        &self,
        link: &Arc<Link>,
        actor: &crate::api::schema::BrowserActor,
        pane: Option<&str>,
        path: &std::path::Path,
        show: ShowRequest<'_>,
        deadline: Duration,
        now: u64,
    ) -> Result<(BrowserRunResult, Option<TabKey>, String), BrowserError> {
        let file = path.display().to_string();
        let Some(shown) = self.inner.link.files().shown(path, link.epoch) else {
            return Err(BrowserError::new(
                "file_not_found",
                format!("no file {file}"),
            ));
        };
        let basename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.clone());
        let reply = self.link_call(
            link,
            "show_remote",
            None,
            json!({
                "tab_id": actor.tab_id(),
                "opened_by": actor.pane_id().map(|pane| format!("agent:{pane}")),
                "token": shown.token,
                "rel": shown.rel,
                "title": show.title,
                "focus": show.focus,
            }),
            deadline,
        )?;
        let target = reply.result["target"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if target.is_empty() {
            return Err(BrowserError::host_failed("show returned no target"));
        }
        let key = TabKey::new(COMPANION_PROFILE, &target);
        let page = reply.page.clone().unwrap_or_default();
        {
            let mut state = self.inner.state.lock().unwrap();
            state.adopt_tab(
                &key,
                &crate::browser::state::HostTab {
                    target: target.clone(),
                    url: page.url.clone(),
                    title: page.title.clone(),
                    selected: show.focus,
                    dialog_open: page.dialog_open,
                },
                actor,
                now,
            );
            if let Some(pane) = pane {
                state.set_cursor(pane, &key, actor.tab_id(), now);
            }
        }
        self.inner.html_views.lock().unwrap().insert(key.clone());
        let record = self.record(&key)?;
        let mut data = json!({
            "tab": record.id(),
            "kind": "html_view",
            "watching": false,
            "file": file,
        });
        if let Some(view) = reply.result["view_id"].as_str() {
            data["view_id"] = json!(view);
        }
        let mut text = format!("showing {basename} (network blocked); {}", text::LOADS_ONCE);
        if show.page_api {
            text.push_str(&format!(" ({})", text::PAGE_API_OFF));
        }
        if show.allow_network {
            text.push_str(&format!(" ({})", text::ALLOW_NETWORK_OFF));
        }
        let result = super::shape::simple_result(&record, Some(&page), &text, data);
        Ok((result, Some(key), format!("show: {basename}")))
    }

    /// Tab groups of gone panes, over the link; at most 64 a frame.
    pub(crate) fn release_over_link(
        &self,
        link: &Arc<Link>,
        keys: &[String],
    ) -> Result<HostReply, BrowserError> {
        let mut answer = Err(unavailable(text::reconnecting(&link.label)));
        let mut failed: Vec<Value> = Vec::new();
        for keys in keys.chunks(RELEASE_MAX_KEYS) {
            let reply = self.link_call(
                link,
                "release",
                None,
                json!({ "keys": keys }),
                Duration::from_secs(5),
            )?;
            failed.extend(
                reply.result["failed"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
            answer = Ok(reply);
        }
        if let Ok(reply) = &mut answer {
            reply.result = json!({ "failed": failed });
        }
        answer
    }

    /// Why there is no browser on a server whose browser is a Mac's; `None`:
    /// this machine's own companion is the answer, as without a link.
    pub(crate) fn no_link_error(&self) -> Option<BrowserError> {
        let last = self.inner.link.last();
        let last = last
            .as_ref()
            .map(|(label, closed)| (label.as_str(), unix_now().saturating_sub(*closed)));
        (last.is_some() || self.config().link_only())
            .then(|| unavailable(text::not_connected(last)))
    }

    /// The link's own lease, while `link` is the live one.
    fn with_link_lease<T>(
        &self,
        link: &Link,
        f: impl FnOnce(&mut CompanionLease) -> T,
    ) -> Option<T> {
        let mut slot = self.inner.companion_host.lock().unwrap();
        slot.as_mut()
            .filter(|lease| {
                lease
                    .link
                    .as_ref()
                    .is_some_and(|held| held.epoch == link.epoch)
            })
            .map(f)
    }

    /// The link's lease goes (with its pages); never a local companion's.
    fn drop_link_lease(&self, link: &Link, why: &str) {
        let dropped = {
            let mut slot = self.inner.companion_host.lock().unwrap();
            let held = slot
                .as_ref()
                .and_then(|lease| lease.link.as_ref())
                .is_some_and(|held| held.epoch == link.epoch);
            held.then(|| slot.take()).flatten()
        };
        if dropped.is_some() {
            tracing::info!(event = "browser.link.host_down", epoch = link.epoch, label = %link.label, reason = %link::clip_text(why), "the linked browser is gone");
            self.release_registration(ProfileStatus::Stopped, false);
            self.companion_changed();
        }
    }
}

impl LinkHandler for BrowserHub {
    /// A sign of life: the lease lives on.
    fn heard(&self, link: &Arc<Link>) {
        self.with_link_lease(link, CompanionLease::renew);
    }

    fn host_up(&self, link: &Arc<Link>, up: HostUp) {
        let instance = up.instance.trim().to_string();
        if instance.is_empty() {
            tracing::warn!(
                event = "browser.link.host_up",
                epoch = link.epoch,
                "host_up without an instance; ignored"
            );
            return;
        }
        let version = link::clip_text(&up.version);
        let now = Instant::now();
        let (lease_id, back) = {
            let mut slot = self.inner.companion_host.lock().unwrap();
            if !self.inner.link.is_live(link) {
                return;
            }
            let old = slot.take().filter(|lease| lease.live(now));
            if old.as_ref().is_some_and(|lease| lease.link.is_none()) {
                // cannot be: a link and a registration exclude each other
                *slot = old;
                tracing::warn!(
                    event = "browser.link.host_up",
                    epoch = link.epoch,
                    "a local companion holds the browser; host_up ignored"
                );
                return;
            }
            let back = old.filter(|lease| lease.instance == instance);
            let same = back.is_some();
            let mut lease = back.unwrap_or_else(|| CompanionLease {
                lease_id: Some(format!("L{}", crate::browser::session::random_hex(24))),
                instance,
                version: String::new(),
                port: 0,
                nonce: String::new(),
                expires_at: now,
                expires_unix: 0,
                last_seq: None,
                report_tokens: REPORT_RATE_PER_S,
                report_refill: now,
                launch_token: None,
                pid: None,
                link: None,
            });
            lease.version = version.clone();
            lease.link = Some(LinkLease {
                epoch: link.epoch,
                host_id: link.host_id.clone(),
                label: link.label.clone(),
                since: link.since.clone(),
            });
            lease.renew();
            let lease_id = lease.lease_id.clone().unwrap_or_default();
            *slot = Some(lease);
            (lease_id, same)
        };
        {
            let mut state = self.inner.state.lock().unwrap();
            state.set_profile(
                COMPANION_PROFILE,
                ProfileStatus::Running {
                    pid: 0,
                    port: 0,
                    since: unix_now(),
                    exe: format!("companion {version} on {}", link.label),
                },
            );
            state.reconcile(COMPANION_PROFILE, &up.tabs, unix_now());
        }
        self.inner.html_views.lock().unwrap().retain(|key| {
            !is_companion(&key.profile) || up.tabs.iter().any(|tab| tab.target == key.target_id)
        });
        self.inner.companion.lock().unwrap().insert(
            COMPANION_PROFILE.to_string(),
            format!("host (on {})", link.label),
        );
        self.start_supervisor();
        tracing::info!(event = "browser.link.host_up", epoch = link.epoch, label = %link.label, %version, electron = %link::clip_text(&up.electron), tabs = up.tabs.len(), back, "the linked browser is up");
        link.answer(&ServerFrame::HostOk { lease_id });
        self.companion_changed();
        self.flush();
    }

    /// A page of a shown file pulls the next piece of it.
    fn file(&self, link: &Arc<Link>, request: &FileRequest) -> FileReply {
        self.inner.link.files().serve(request, link.epoch)
    }

    fn host_down(&self, link: &Arc<Link>, reason: &str) {
        self.drop_link_lease(link, reason);
    }

    /// A sidecar event of the linked browser: always the companion profile's,
    /// whatever it says.
    fn event(&self, link: &Arc<Link>, mut event: Value) {
        if self.with_link_lease(link, |_| ()).is_none() {
            return;
        }
        let Some(fields) = event.as_object_mut() else {
            return;
        };
        fields.insert("profile".into(), json!(COMPANION_PROFILE));
        // a shown file's page is the file here
        let file_url = fields
            .get("url")
            .and_then(Value::as_str)
            .and_then(|url| self.inner.link.files().to_file_url(url));
        if let Some(url) = file_url {
            fields.insert("url".into(), json!(url));
        }
        match serde_json::from_value::<HostEvent>(event) {
            // its browser's coming and going is host_up, host_down and the link
            Ok(HostEvent::Browser { .. }) => {}
            Ok(event) => self.handle_event(event),
            Err(err) => {
                tracing::debug!(event = "browser.link.event", epoch = link.epoch, %err, "not a sidecar event; ignored")
            }
        }
    }

    fn closed(&self, link: &Arc<Link>, end: &LinkEnd) {
        // the last client quit there and its browser with it; a dropped link's
        // lease runs out by itself
        if matches!(end, LinkEnd::Bye(said) if said.code == bye::QUIT) {
            self.drop_link_lease(link, "the helper quit");
        }
        self.companion_changed();
    }
}

#[cfg(test)]
impl BrowserHub {
    /// Tests: the local sidecar answers request `id`.
    pub(crate) fn test_local_reply(&self, id: u64) {
        self.handle_message(super::host::HostMessage::Reply(HostReply {
            id,
            ok: true,
            result: json!({}),
            error: None,
            page: None,
        }));
    }

    /// Tests: the local sidecar says this.
    pub(crate) fn test_local_event(&self, event: Value) {
        if let Ok(event) = serde_json::from_value::<HostEvent>(event) {
            self.handle_event(event);
        }
    }

    /// Tests: the local sidecar is gone.
    pub(crate) fn drop_fake_host_for_test(&self) {
        let generation = self
            .inner
            .host
            .lock()
            .unwrap()
            .as_ref()
            .map(|host| host.generation);
        if let Some(generation) = generation {
            self.drop_host(generation, Some("test".into()));
        }
    }
}

#[cfg(all(test, unix))]
#[path = "link_hub/tests.rs"]
mod tests;
