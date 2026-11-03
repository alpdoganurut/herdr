//! The browser host role (fork, companion browser): an external browser
//! (the Electron companion, `apps/companion`) registers its DevTools
//! endpoint and holds a lease by heartbeat; while the lease lives, the
//! reserved profile `companion` is that browser. Everything downstream
//! (sidecar attach, tab records, short ids `companion:t3`, cursors, the
//! ledger) is the ordinary profile machinery.
//!
//! A child module of `hub` (it reaches the hub's private state). Register,
//! heartbeat and unregister run on the caller's connection thread; the
//! supervisor expires the lease; the App lane checks a lease id for
//! `browser.host_state` / `host_sync` / `host_report`.
//!
//! Liveness never probes or signals the host's pid (remote-ready): the
//! lease, the sidecar's `disconnected` event and `host_unregister` are the
//! only ways it goes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{BrowserHub, ATTACH_TIMEOUT};
use crate::api::schema::{
    BrowserHostKind, BrowserHostLeaseInfo, BrowserHostRegisterParams, BrowserOp,
};
use crate::browser::state::{HostTab, ProfileStatus};
use crate::browser::{unix_now, BrowserError};
use crate::config::{BrowserConfig, BrowserHostMode};

/// The reserved profile a registered host serves.
pub const COMPANION_PROFILE: &str = "companion";
/// The host protocol this server speaks (`browser.host_register.protocol`).
pub const COMPANION_HOST_PROTOCOL: u32 = 1;
/// How often the host heartbeats.
pub const HEARTBEAT_MS: u64 = 5_000;
/// A lease not renewed within this goes.
pub const LEASE_TTL_MS: u64 = 15_000;
/// What the lease answer advertises beyond the register methods.
pub const SERVER_FEATURES: [&str; 6] = [
    "host_sync",
    "host_report",
    "send_page",
    "show",
    "page_errors",
    // Fork (companion launcher): browser.placements / browser.placement_report.
    "placements",
];
/// The DevTools probe of a registering host.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// A pending registration (the attach under way, at most twice
/// `ATTACH_TIMEOUT` plus the probe) goes after this even when its
/// register call never returns, so a companion that died mid-attach does
/// not block the next one for good.
pub(super) const PENDING_TIMEOUT: Duration = Duration::from_secs(ATTACH_TIMEOUT.as_secs() * 2 + 10);
/// `browser.host_report`: at most this many events per second per lease
/// (token bucket, burst of the same size).
pub const REPORT_RATE_PER_S: f64 = 20.0;
/// `browser.host_report`: at most this many events per request.
pub const REPORT_MAX_EVENTS: usize = 50;

/// The registered host (one at a time).
#[derive(Debug, Clone)]
pub(super) struct CompanionLease {
    /// `None` while the registration is pending (attach under way): it
    /// already blocks other instances, until `expires_at`
    /// (`PENDING_TIMEOUT` after it started).
    pub lease_id: Option<String>,
    pub instance: String,
    pub version: String,
    pub port: u16,
    pub nonce: String,
    pub expires_at: Instant,
    pub expires_unix: u64,
    /// The last applied `browser.host_sync` seq.
    pub last_seq: Option<u64>,
    /// `browser.host_report` token bucket: tokens and the last refill.
    pub report_tokens: f64,
    pub report_refill: Instant,
    /// Fork (companion launcher): the launch token the registration
    /// carried; the launcher's own token makes the lease managed.
    pub launch_token: Option<String>,
    /// The process id the registration named (informational).
    pub pid: Option<u32>,
}

impl CompanionLease {
    fn live(&self, now: Instant) -> bool {
        self.expires_at > now
    }
}

/// Whether `host` is a loopback address (v1 accepts only those).
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// `profile` is the reserved companion profile.
pub fn is_companion(profile: &str) -> bool {
    profile == COMPANION_PROFILE
}

impl BrowserHub {
    /// `browser.host_register`. Blocking (the sidecar attach, up to twice
    /// `ATTACH_TIMEOUT`); runs on the caller's connection thread.
    pub fn host_register(
        &self,
        params: &BrowserHostRegisterParams,
    ) -> Result<BrowserHostLeaseInfo, BrowserError> {
        let config = self.config();
        if !config.enabled {
            return Err(BrowserError::disabled());
        }
        // Fork (companion launcher): only a herdr whose window is on and
        // chosen refuses hosts.
        if config.window && config.host_mode() == BrowserHostMode::Window {
            return Err(BrowserError::new(
                "host_disabled",
                "this herdr uses only its own browser window ([browser] host = \"window\")",
            ));
        }
        if params.protocol != COMPANION_HOST_PROTOCOL {
            return Err(BrowserError::new(
                "host_protocol_unsupported",
                format!(
                    "host protocol {} is not supported; this herdr speaks {COMPANION_HOST_PROTOCOL}",
                    params.protocol
                ),
            ));
        }
        let invalid = |what: &str| BrowserError::new("invalid_request", what.to_string());
        if params.kind == BrowserHostKind::Unknown {
            return Err(invalid("unknown host kind"));
        }
        if params.instance.trim().is_empty() || params.nonce.trim().is_empty() {
            return Err(invalid("instance and nonce are required"));
        }
        if !is_loopback_host(&params.cdp.host) || params.cdp.port == 0 {
            return Err(invalid(
                "the DevTools endpoint must be a loopback address with a port",
            ));
        }
        let now = Instant::now();
        let replaced_port = {
            let mut slot = self.inner.companion_host.lock().unwrap();
            let mut replaced_port = None;
            if let Some(lease) = slot.as_ref() {
                if lease.live(now) && lease.instance != params.instance {
                    return Err(BrowserError::new(
                        "host_in_use",
                        format!(
                            "another companion is registered (instance {}, version {}); close it first",
                            lease.instance,
                            if lease.version.is_empty() { "?" } else { &lease.version }
                        ),
                    ));
                }
                replaced_port = Some(lease.port);
            }
            *slot = Some(CompanionLease {
                lease_id: None,
                instance: params.instance.clone(),
                version: params.version.clone(),
                port: params.cdp.port,
                nonce: params.nonce.clone(),
                expires_at: now + PENDING_TIMEOUT,
                expires_unix: 0,
                last_seq: None,
                report_tokens: REPORT_RATE_PER_S,
                report_refill: now,
                launch_token: params
                    .launch_token
                    .as_deref()
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_string),
                pid: params.pid,
            });
            replaced_port
        };
        let test_mode = self
            .inner
            .test_mode
            .load(std::sync::atomic::Ordering::Relaxed);
        if !test_mode && super::launch::json_version(params.cdp.port, PROBE_TIMEOUT).is_none() {
            self.drop_registration(ProfileStatus::Stopped, false);
            return Err(BrowserError::new(
                "host_unreachable",
                format!(
                    "nothing answers DevTools on 127.0.0.1:{} (start the companion with remote debugging)",
                    params.cdp.port
                ),
            ));
        }
        // A same-instance re-register on another port: the old attach goes.
        if replaced_port.is_some_and(|port| port != params.cdp.port) {
            self.detach_companion();
        }
        self.inner.state.lock().unwrap().set_profile(
            COMPANION_PROFILE,
            ProfileStatus::Running {
                pid: params.pid.unwrap_or(0),
                port: params.cdp.port,
                since: unix_now(),
                exe: if params.version.is_empty() {
                    "companion".into()
                } else {
                    format!("companion {}", params.version)
                },
            },
        );
        if let Err(err) = self.ensure_running(COMPANION_PROFILE) {
            self.drop_registration(ProfileStatus::Stopped, true);
            return Err(err);
        }
        let lease_id = format!("L{}", super::super::session::random_hex(24));
        let ttl = Duration::from_millis(LEASE_TTL_MS);
        let expires_unix = unix_now() + LEASE_TTL_MS / 1000;
        {
            let mut slot = self.inner.companion_host.lock().unwrap();
            match slot.as_mut() {
                Some(lease) if lease.instance == params.instance => {
                    lease.lease_id = Some(lease_id.clone());
                    lease.expires_at = Instant::now() + ttl;
                    lease.expires_unix = expires_unix;
                }
                other => {
                    // Expired (the supervisor dropped it) or replaced by
                    // another instance while the attach ran.
                    let expired = other.is_none();
                    drop(slot);
                    if expired {
                        self.detach_companion();
                    }
                    return Err(BrowserError::new(
                        "host_in_use",
                        "the registration timed out or was replaced while it attached; retry",
                    ));
                }
            }
        }
        // Fork (companion launcher): the launcher's own process is managed.
        let managed = self.companion_registered(params.launch_token.as_deref());
        tracing::info!(event = "browser.host.register", instance = %params.instance, version = %params.version, port = params.cdp.port, managed, "companion browser registered");
        self.flush();
        Ok(lease_info(
            &lease_id,
            expires_unix,
            managed.then(|| self.companion_update_ready()).flatten(),
        ))
    }

    /// `browser.host_heartbeat`: a fresh expiry, or `lease_unknown`.
    pub fn host_heartbeat(&self, lease_id: &str) -> Result<BrowserHostLeaseInfo, BrowserError> {
        let now = Instant::now();
        let (expires_unix, token) = {
            let mut slot = self.inner.companion_host.lock().unwrap();
            match slot.as_mut() {
                Some(lease) if lease.lease_id.as_deref() == Some(lease_id) && lease.live(now) => {
                    lease.expires_at = now + Duration::from_millis(LEASE_TTL_MS);
                    lease.expires_unix = unix_now() + LEASE_TTL_MS / 1000;
                    (lease.expires_unix, lease.launch_token.clone())
                }
                _ => return Err(lease_unknown()),
            }
        };
        // Fork (companion launcher): `update` while newer app files wait.
        let managed = token.is_some() && token == self.launcher().lock().token;
        Ok(lease_info(
            lease_id,
            expires_unix,
            managed.then(|| self.companion_update_ready()).flatten(),
        ))
    }

    /// `browser.host_unregister`: the companion is closing.
    pub fn host_unregister(&self, lease_id: &str) -> Result<(), BrowserError> {
        if !self.lease_matches(lease_id) {
            return Err(lease_unknown());
        }
        // Fork (companion launcher): the process quits on its own (not a crash).
        let token = self
            .inner
            .companion_host
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|lease| lease.launch_token.clone());
        self.companion_unregistered(token.as_deref());
        self.drop_registration(ProfileStatus::Stopped, true);
        tracing::info!(
            event = "browser.host.unregister",
            "companion browser unregistered"
        );
        Ok(())
    }

    /// Whether `lease_id` names the live lease (the App lane's check).
    pub fn lease_matches(&self, lease_id: &str) -> bool {
        let now = Instant::now();
        self.inner
            .companion_host
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|lease| lease.lease_id.as_deref() == Some(lease_id) && lease.live(now))
    }

    /// Whether a companion holds a live lease (routing).
    pub fn companion_live(&self) -> bool {
        let now = Instant::now();
        self.inner
            .companion_host
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|lease| lease.lease_id.is_some() && lease.live(now))
    }

    /// `browser.host_sync`'s ordering check: `stale_seq` for a seq at or
    /// below the last applied one, `lease_unknown` for another lease.
    pub fn accept_host_sync_seq(&self, lease_id: &str, seq: u64) -> Result<(), BrowserError> {
        let now = Instant::now();
        let mut slot = self.inner.companion_host.lock().unwrap();
        let Some(lease) = slot
            .as_mut()
            .filter(|lease| lease.lease_id.as_deref() == Some(lease_id) && lease.live(now))
        else {
            return Err(lease_unknown());
        };
        if lease.last_seq.is_some_and(|last| seq <= last) {
            return Err(BrowserError::new(
                "stale_seq",
                format!("sync {seq} is not newer than the last applied one; ignored"),
            ));
        }
        lease.last_seq = Some(seq);
        Ok(())
    }

    /// `browser.host_report`'s rate limit: how many of `count` events the
    /// lease's bucket admits now (the rest are dropped, never an error).
    pub fn admit_host_report(&self, lease_id: &str, count: usize) -> Result<usize, BrowserError> {
        let now = Instant::now();
        let mut slot = self.inner.companion_host.lock().unwrap();
        let Some(lease) = slot
            .as_mut()
            .filter(|lease| lease.lease_id.as_deref() == Some(lease_id) && lease.live(now))
        else {
            return Err(lease_unknown());
        };
        let elapsed = now.duration_since(lease.report_refill).as_secs_f64();
        lease.report_tokens =
            (lease.report_tokens + elapsed * REPORT_RATE_PER_S).min(REPORT_RATE_PER_S);
        lease.report_refill = now;
        let admitted = (lease.report_tokens.floor() as usize).min(count.min(REPORT_MAX_EVENTS));
        lease.report_tokens -= admitted as f64;
        Ok(admitted)
    }

    /// The profile `open` / `show` without `--profile` use (`[browser] host`).
    /// Fork (companion launcher): the companion without a live lease is
    /// started and waited for (on the caller's connection thread); `user`:
    /// the user's own call (errors say what to run).
    pub(super) fn host_profile(
        &self,
        config: &BrowserConfig,
        user: bool,
    ) -> Result<String, BrowserError> {
        // Fork (companion launcher): with the herdr+ window off every page
        // goes to the companion (`auto` falls back only to an enabled window).
        if !config.window {
            if config.host_mode() == BrowserHostMode::Window {
                return Err(window_disabled());
            }
            if !self.companion_live() {
                self.ensure_companion(self.companion_register_wait(), user)?;
            }
            return Ok(COMPANION_PROFILE.into());
        }
        match config.host_mode() {
            BrowserHostMode::Window => Ok(config.default_profile().to_string()),
            BrowserHostMode::Auto if self.companion_live() => Ok(COMPANION_PROFILE.into()),
            BrowserHostMode::Auto => Ok(config.default_profile().to_string()),
            BrowserHostMode::Companion if self.companion_live() => Ok(COMPANION_PROFILE.into()),
            BrowserHostMode::Companion => {
                self.ensure_companion(self.companion_register_wait(), user)?;
                Ok(COMPANION_PROFILE.into())
            }
        }
    }

    /// How long routing waits for the companion to register.
    pub(crate) fn companion_register_wait(&self) -> Duration {
        let ms = self
            .inner
            .companion_wait_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        if ms == 0 {
            crate::browser::companion_app::REGISTER_WAIT
        } else {
            Duration::from_millis(ms)
        }
    }

    /// Tests: a shorter routing wait.
    #[cfg(test)]
    pub fn set_companion_register_wait_for_test(&self, wait: Duration) {
        self.inner.companion_wait_ms.store(
            wait.as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Fork (companion launcher): the lease as the launcher needs it.
    pub(crate) fn companion_lease_facts(&self) -> crate::browser::companion_app::LeaseFacts {
        let now = Instant::now();
        let slot = self.inner.companion_host.lock().unwrap();
        match slot.as_ref() {
            Some(lease) => crate::browser::companion_app::LeaseFacts {
                live: lease.lease_id.is_some() && lease.live(now),
                launch_token: lease.launch_token.clone(),
                version: lease.version.clone(),
                pid: lease.pid.filter(|pid| *pid != 0),
            },
            None => Default::default(),
        }
    }

    /// Fork (companion launcher): a companion started by hand is only
    /// detached and its lease dropped (it re-registers); never signalled.
    pub(crate) fn detach_external_companion(&self) {
        self.drop_registration(ProfileStatus::Stopped, true);
    }

    /// Fork (companion launcher): the managed process with `token` is gone:
    /// its lease goes now, so the next launch registers at once instead of
    /// meeting `host_in_use` until the lease expires.
    pub(crate) fn drop_companion_lease_for(&self, token: &str, crashed: bool) {
        let taken = {
            let mut slot = self.inner.companion_host.lock().unwrap();
            if slot
                .as_ref()
                .is_some_and(|lease| lease.launch_token.as_deref() == Some(token))
            {
                slot.take()
            } else {
                None
            }
        };
        if taken.is_some() {
            self.release_registration(
                if crashed {
                    ProfileStatus::Crashed {
                        at: unix_now(),
                        detail: "companion gone".into(),
                    }
                } else {
                    ProfileStatus::Stopped
                },
                true,
            );
        }
    }

    /// Whether `op` creates a page and so follows `[browser] host` when no
    /// profile is named (a batch whose first step does).
    pub(super) fn op_follows_host(op: &BrowserOp) -> bool {
        match op {
            BrowserOp::Open { .. } | BrowserOp::Show { .. } => true,
            BrowserOp::Batch { ops, .. } => ops.first().is_some_and(|step| {
                step.tab.is_none()
                    && matches!(step.op, BrowserOp::Open { .. } | BrowserOp::Show { .. })
            }),
            _ => false,
        }
    }

    /// `ensure_running` for the companion profile: never launches; attaches
    /// the sidecar (kind companion, the nonce) while a registration lives.
    pub(super) fn ensure_companion_attached(&self) -> Result<(), BrowserError> {
        let (port, nonce) = {
            let now = Instant::now();
            let slot = self.inner.companion_host.lock().unwrap();
            match slot.as_ref().filter(|lease| lease.live(now)) {
                Some(lease) => (lease.port, lease.nonce.clone()),
                None => return Err(host_unavailable()),
            }
        };
        let already_attached = {
            let running = self
                .inner
                .state
                .lock()
                .unwrap()
                .profile(COMPANION_PROFILE)
                .is_running();
            running
                && self
                    .inner
                    .host
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|link| link.attached.contains(COMPANION_PROFILE))
        };
        if already_attached {
            return Ok(());
        }
        self.ensure_host()?;
        let args = json!({ "port": port, "kind": "companion", "nonce": nonce });
        let mut attempt = 0;
        let reply = loop {
            attempt += 1;
            match self.request(
                "attach",
                Some(COMPANION_PROFILE),
                None,
                args.clone(),
                ATTACH_TIMEOUT,
            ) {
                Ok(reply) => break reply,
                Err(err)
                    if (err.code == "browser_timeout" || err.code == "attach_timeout")
                        && attempt < 2 =>
                {
                    tracing::warn!(event = "browser.host.attach.retry", message = %err.message, "companion attach timed out; retrying once");
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(err) if err.code == "browser_timeout" || err.code == "attach_timeout" => {
                    return Err(BrowserError::new(
                        "attach_blocked",
                        format!(
                            "the companion did not accept the attach within {} s (twice): {}",
                            ATTACH_TIMEOUT.as_secs(),
                            err.message
                        ),
                    ))
                }
                Err(err) => return Err(err),
            }
        };
        let tabs: Vec<HostTab> =
            serde_json::from_value(reply.result["tabs"].clone()).unwrap_or_default();
        self.inner
            .state
            .lock()
            .unwrap()
            .reconcile(COMPANION_PROFILE, &tabs, unix_now());
        if let Some(link) = self.inner.host.lock().unwrap().as_mut() {
            link.attached.insert(COMPANION_PROFILE.to_string());
        }
        let detail = reply.result["companion"]["detail"]
            .as_str()
            .filter(|detail| !detail.is_empty())
            .map(|detail| format!("host ({detail})"))
            .unwrap_or_else(|| "host".into());
        self.inner
            .companion
            .lock()
            .unwrap()
            .insert(COMPANION_PROFILE.to_string(), detail);
        self.flush();
        Ok(())
    }

    /// The registration goes: lease removed, the sidecar detached (when
    /// `detach`), the companion's tabs closed in the ledger, the profile
    /// set to `status`. `Tab.browser` state is kept (the restore source).
    pub(super) fn drop_registration(&self, status: ProfileStatus, detach: bool) {
        self.inner.companion_host.lock().unwrap().take();
        self.release_registration(status, detach);
    }

    /// [`Self::drop_registration`] after the lease slot was emptied.
    fn release_registration(&self, status: ProfileStatus, detach: bool) {
        // The companion's views go with it (the sidecar's disconnect path
        // emits no `closed` for them).
        self.inner
            .html_views
            .lock()
            .unwrap()
            .retain(|key| !is_companion(&key.profile));
        if detach {
            self.detach_companion();
        }
        self.inner
            .companion
            .lock()
            .unwrap()
            .remove(COMPANION_PROFILE);
        {
            let mut state = self.inner.state.lock().unwrap();
            state.close_profile_tabs(COMPANION_PROFILE, unix_now());
            state.set_profile(COMPANION_PROFILE, status);
        }
        self.flush();
    }

    /// Detach the sidecar from the companion (the app stays up).
    fn detach_companion(&self) {
        let attached = self
            .inner
            .host
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|link| link.attached.remove(COMPANION_PROFILE));
        if attached {
            let _ = self.request(
                "detach",
                Some(COMPANION_PROFILE),
                None,
                Value::Null,
                Duration::from_secs(3),
            );
        }
    }

    /// Supervisor: a lease past its expiry goes (the companion is gone).
    pub(super) fn expire_host_lease(&self, now: Instant) {
        // The check and the take under one guard: a register that lands
        // in between replaces the slot and is never dropped here.
        let expired = {
            let mut slot = self.inner.companion_host.lock().unwrap();
            if slot.as_ref().is_some_and(|lease| !lease.live(now)) {
                slot.take()
            } else {
                None
            }
        };
        if expired.is_some() {
            tracing::info!(
                event = "browser.host.expired",
                "companion lease (or pending registration) expired"
            );
            self.release_registration(
                ProfileStatus::Crashed {
                    at: unix_now(),
                    detail: "companion gone".into(),
                },
                true,
            );
        }
    }

    /// The sidecar lost the companion's CDP connection: the lease goes now.
    pub(super) fn companion_disconnected(&self) {
        let registered = self.inner.companion_host.lock().unwrap().is_some();
        if !registered {
            return;
        }
        tracing::info!(
            event = "browser.host.disconnected",
            "companion CDP connection closed"
        );
        self.drop_registration(
            ProfileStatus::Crashed {
                at: unix_now(),
                detail: "companion gone".into(),
            },
            false,
        );
    }

    /// Tests: a live lease without a sidecar attach (App-lane tests).
    #[cfg(test)]
    pub fn install_test_lease(&self, lease_id: &str) {
        let now = Instant::now();
        *self.inner.companion_host.lock().unwrap() = Some(CompanionLease {
            lease_id: Some(lease_id.to_string()),
            instance: "test".into(),
            version: String::new(),
            port: 1,
            nonce: "n".into(),
            expires_at: now + Duration::from_secs(3600),
            expires_unix: unix_now() + 3600,
            last_seq: None,
            report_tokens: REPORT_RATE_PER_S,
            report_refill: now,
            launch_token: None,
            pid: None,
        });
    }

    /// Tests: a registration left pending (its register call never
    /// returned) that started `ago`.
    #[cfg(test)]
    pub fn install_pending_registration_for_test(&self, instance: &str, ago: Duration) {
        let now = Instant::now();
        *self.inner.companion_host.lock().unwrap() = Some(CompanionLease {
            lease_id: None,
            instance: instance.into(),
            version: String::new(),
            port: 1,
            nonce: "n".into(),
            expires_at: (now + PENDING_TIMEOUT).checked_sub(ago).unwrap_or(now),
            expires_unix: 0,
            last_seq: None,
            report_tokens: REPORT_RATE_PER_S,
            report_refill: now,
            launch_token: None,
            pid: None,
        });
    }

    /// Tests: how many known HTML views `profile` has.
    #[cfg(test)]
    pub fn html_views_for_test(&self, profile: &str) -> usize {
        self.inner
            .html_views
            .lock()
            .unwrap()
            .iter()
            .filter(|key| key.profile == profile)
            .count()
    }

    /// Tests: the live lease's id.
    #[cfg(test)]
    pub fn companion_lease_id_for_test(&self) -> Option<String> {
        self.inner
            .companion_host
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|lease| lease.lease_id.clone())
    }

    /// Tests: the lease runs out now (the next supervisor pass drops it).
    #[cfg(test)]
    pub fn expire_host_lease_for_test(&self) {
        if let Some(lease) = self.inner.companion_host.lock().unwrap().as_mut() {
            lease.expires_at = Instant::now() - Duration::from_secs(1);
        }
    }
}

fn lease_info(lease_id: &str, expires_unix: u64, update: Option<String>) -> BrowserHostLeaseInfo {
    BrowserHostLeaseInfo {
        lease_id: lease_id.to_string(),
        profile: COMPANION_PROFILE.into(),
        state: "attached".into(),
        heartbeat_ms: HEARTBEAT_MS,
        ttl_ms: LEASE_TTL_MS,
        expires_unix,
        host_protocol: COMPANION_HOST_PROTOCOL,
        server_features: SERVER_FEATURES.iter().map(|f| f.to_string()).collect(),
        update,
    }
}

pub fn lease_unknown() -> BrowserError {
    BrowserError::new(
        "lease_unknown",
        "no live companion lease with that id; register again",
    )
}

pub fn host_unavailable() -> BrowserError {
    BrowserError::new(
        "host_unavailable",
        "the herdr+ companion browser is not running; `herdr browser companion start` starts it (or set [browser] window = true and host = \"auto\" for the herdr+ window)",
    )
}

/// Fork (companion launcher): an op on a window profile while the herdr+
/// window is off (`[browser] window = false`).
pub fn window_disabled() -> BrowserError {
    BrowserError::new(
        "window_disabled",
        "the herdr+ browser window is off ([browser] window = false); agents use the companion browser. Set [browser] window = true to use the window again.",
    )
}

/// `browser.run show`'s arguments.
pub(super) struct ShowRequest<'a> {
    pub file: &'a str,
    pub allow_network: bool,
    pub watch: bool,
    pub focus: bool,
    pub title: Option<&'a str>,
}

/// The file `show` may open: absolute, existing, `.html` / `.htm`.
pub fn check_show_file(file: &str) -> Result<std::path::PathBuf, BrowserError> {
    let path = std::path::Path::new(file.trim());
    if !path.is_absolute() {
        return Err(BrowserError::new(
            "invalid_request",
            format!("show needs an absolute path, not {file:?}"),
        ));
    }
    let html = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"));
    if !path.is_file() {
        return Err(BrowserError::new(
            "file_not_found",
            format!("no file {}", path.display()),
        ));
    }
    if !html {
        return Err(BrowserError::new(
            "not_html",
            format!("{} is not an .html or .htm file", path.display()),
        ));
    }
    Ok(path.to_path_buf())
}

impl BrowserHub {
    /// `show`: a sandboxed HTML view in the companion (the caller's
    /// group), else a window tab with the network routed off; the sidecar
    /// watches the file and reloads. The view becomes the caller's tab.
    pub(super) fn execute_show(
        &self,
        actor: &crate::api::schema::BrowserActor,
        profile: &str,
        pane: Option<&str>,
        show: ShowRequest<'_>,
        deadline: Duration,
        now: u64,
    ) -> Result<
        (
            crate::api::schema::BrowserRunResult,
            Option<super::TabKey>,
            String,
        ),
        BrowserError,
    > {
        let path = check_show_file(show.file)?;
        let file = path.display().to_string();
        // The companion resolves symlinks for the root and every file it
        // serves: the root is the directory the file really lives in.
        let root = std::fs::canonicalize(&path)
            .ok()
            .as_deref()
            .and_then(std::path::Path::parent)
            .or_else(|| path.parent())
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        let basename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.clone());
        let reply = self.request(
            "show",
            Some(profile),
            None,
            json!({
                "tab_id": actor.tab_id(),
                "opened_by": actor.pane_id().map(|pane| format!("agent:{pane}")),
                "file": file,
                "root": root,
                "allow_network": show.allow_network,
                "watch": show.watch,
                "focus": show.focus,
                "title": show.title,
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
        let key = super::TabKey::new(profile, &target);
        let page = reply.page.clone().unwrap_or_default();
        {
            let mut state = self.inner.state.lock().unwrap();
            state.adopt_tab(
                &key,
                &HostTab {
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
        let watching = reply.result["watching"].as_bool().unwrap_or(false);
        let mut data = json!({
            "tab": record.id(),
            "kind": "html_view",
            "watching": watching,
            "file": file,
        });
        if let Some(view) = reply.result["view_id"].as_str() {
            data["view_id"] = json!(view);
        }
        let text = format!(
            "showing {basename}{}{}",
            if show.allow_network {
                ""
            } else {
                " (network blocked)"
            },
            if watching {
                "; it reloads when the file changes"
            } else {
                ""
            }
        );
        let result = super::shape::simple_result(&record, Some(&page), &text, data);
        Ok((result, Some(key), format!("show: {basename}")))
    }

    /// Queue a page error for the agent that uses the tab (`[browser]
    /// page_errors`); called with the ledger locked, never blocks.
    pub(super) fn queue_page_error(
        &self,
        state: &crate::browser::state::BrowserState,
        event: &crate::browser::state::HostTabEvent,
        now_unix: u64,
    ) {
        // Per console error, under the ledger lock: two fields read in place, no
        // config clone.
        let (to_agents, active_seconds) = {
            let config = self.inner.config.read().unwrap();
            (config.page_errors_to_agents(), config.active_seconds)
        };
        if !to_agents {
            return;
        }
        let key = super::TabKey::new(&event.profile, &event.target);
        let Some(record) = state.tabs.get(&key).filter(|record| record.is_open()) else {
            return;
        };
        // The opener of an HTML view; else whoever used the tab lately,
        // else its opener.
        let recent = record
            .last_actor
            .pane_id()
            .filter(|_| now_unix.saturating_sub(record.last_at) <= active_seconds)
            .filter(|_| !self.inner.html_views.lock().unwrap().contains(&key));
        let Some(pane) = recent.or_else(|| record.opened_by.pane_id()) else {
            return;
        };
        let text = event
            .text
            .clone()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| match event.kind.as_str() {
                "crashed" => "the page crashed".into(),
                "load_failed" => "the page failed to load".into(),
                _ => "(no message)".into(),
            });
        self.inner.page_errors.lock().unwrap().note(
            pane,
            &key,
            crate::browser::page_errors::PageErrorTab {
                tab: record.id(),
                url: record.url.clone(),
                title: record.title.clone(),
            },
            &event.kind,
            &text,
            Instant::now(),
        );
    }

    /// A page error the companion reported for one of its views.
    pub fn note_host_page_error(&self, target: &str, kind: &str, text: &str) {
        let kind = match kind {
            "load_failed" | "crashed" => kind,
            _ => "page_error",
        };
        let event = crate::browser::state::HostTabEvent {
            profile: COMPANION_PROFILE.into(),
            target: target.to_string(),
            kind: kind.into(),
            url: String::new(),
            title: String::new(),
            opener: None,
            initiator: String::new(),
            selected: None,
            text: Some(text.chars().take(300).collect()),
        };
        let mut state = self.inner.state.lock().unwrap();
        state.apply_tab_event(&event, unix_now());
        self.queue_page_error(&state, &event, unix_now());
        let key = super::TabKey::new(COMPANION_PROFILE, target);
        if kind == "load_failed" && self.inner.html_views.lock().unwrap().contains(&key) {
            state.set_attention(
                &key,
                crate::browser::state::BrowserAttentionKind::LoadFailed,
                unix_now(),
            );
        }
    }

    /// Whether page errors wait for their batch window.
    pub fn page_errors_pending(&self) -> bool {
        !self.inner.page_errors.lock().unwrap().is_empty()
    }

    /// The page error batches due now (the App's housekeeping tick).
    pub fn take_page_errors(&self) -> Vec<crate::browser::page_errors::PageErrorBatch> {
        self.take_page_errors_at(Instant::now())
    }

    /// [`Self::take_page_errors`] at a given time (tests skip the window).
    pub fn take_page_errors_at(
        &self,
        now: Instant,
    ) -> Vec<crate::browser::page_errors::PageErrorBatch> {
        let mut queue = self.inner.page_errors.lock().unwrap();
        if queue.is_empty() {
            return Vec::new();
        }
        queue.take_due(now)
    }

    /// The user saw these companion targets (`host_report` `shown`).
    pub fn clear_attention(&self, profile: &str, targets: &[String]) {
        let keys: Vec<super::TabKey> = targets
            .iter()
            .map(|target| super::TabKey::new(profile, target))
            .collect();
        if keys.is_empty() {
            return;
        }
        // persisted by the supervisor's next pass (the App calls this)
        self.inner.state.lock().unwrap().clear_attention(&keys);
    }

    /// The open targets of `profile` that need the user.
    pub fn targets_needing_user(&self, profile: &str) -> std::collections::HashSet<String> {
        self.inner
            .state
            .lock()
            .unwrap()
            .tabs
            .values()
            .filter(|record| record.profile == profile && record.is_open())
            .filter(|record| record.attention.is_some())
            .map(|record| record.target_id.clone())
            .collect()
    }

    /// A user action in the companion's strip, in the ledger as the user
    /// (`user:<action>`).
    pub fn record_user_action(&self, target: Option<&str>, action: &str, url: Option<&str>) {
        let now = unix_now();
        let key = target
            .filter(|target| !target.is_empty())
            .map(|target| super::TabKey::new(COMPANION_PROFILE, target));
        let detail = url
            .map(crate::browser::state::display_url)
            .unwrap_or_default();
        let mut state = self.inner.state.lock().unwrap();
        let key = key.filter(|key| state.tabs.contains_key(key));
        state.touch(
            COMPANION_PROFILE,
            key.as_ref(),
            &crate::api::schema::BrowserActor::User,
            &format!("user:{action}"),
            &detail,
            true,
            0,
            now,
        );
    }

    /// Fork (companion launcher): a `browser.companion` action in the
    /// ledger, as `companion:<action>` by its actor.
    pub fn record_companion_action(
        &self,
        actor: &crate::api::schema::BrowserActor,
        action: &str,
        ok: bool,
        detail: &str,
    ) {
        let mut state = self.inner.state.lock().unwrap();
        state.touch(
            COMPANION_PROFILE,
            None,
            actor,
            &format!("companion:{action}"),
            detail,
            ok,
            0,
            unix_now(),
        );
        drop(state);
        self.flush();
    }

    /// `browser.get` must re-pull: a herdr tab's browser group changed.
    pub fn note_session_change(&self) {
        self.inner.state.lock().unwrap().bump_seq();
    }

    /// Fork (companion launcher): the companion pages whose target is new
    /// (`(target, "<group key>/<entry id>")`): a page the companion
    /// restored keeps its ledger record (id, attribution, cursors).
    pub fn bind_companion_entries(&self, pairs: &[(String, String)]) {
        if pairs.is_empty() {
            return;
        }
        self.inner
            .state
            .lock()
            .unwrap()
            .bind_entries(COMPANION_PROFILE, pairs);
    }

    /// The most recent pane that used the companion target (ledger
    /// `users`, most recent first).
    pub fn last_user_of(&self, target: &str) -> Option<String> {
        let state = self.inner.state.lock().unwrap();
        let key = super::TabKey::new(COMPANION_PROFILE, target);
        state
            .tabs
            .get(&key)
            .and_then(|record| record.users.first().cloned())
    }

    /// After a restart the companion recreated its HTML views: watch them
    /// again (off the caller's thread).
    /// A target already known as an HTML view (a `show` this run, which
    /// armed its own watch or was asked not to) is never re-armed.
    pub fn rewatch_html_views(&self, views: Vec<(String, String)>) {
        if views.is_empty() || self.inner.host.lock().unwrap().is_none() {
            return;
        }
        let views: Vec<(String, String)> = {
            let mut known = self.inner.html_views.lock().unwrap();
            views
                .into_iter()
                .filter(|(target, _)| known.insert(super::TabKey::new(COMPANION_PROFILE, target)))
                .collect()
        };
        if views.is_empty() {
            return;
        }
        let hub = self.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-browser-watch".into())
            .spawn(move || {
                for (target, file) in views {
                    if let Err(err) = hub.request(
                        "watch",
                        Some(COMPANION_PROFILE),
                        Some(&target),
                        json!({ "file": file }),
                        Duration::from_secs(5),
                    ) {
                        tracing::debug!(event = "browser.watch", code = %err.code, "re-watch failed");
                    }
                }
            });
    }
}
