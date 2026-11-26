//! The real companion and sidecar of a helper, made from the machine's
//! parts ([`super::helper::MachineParts`]): `companion::Companion` with the
//! process spawner and `sidecar::Sidecar` with the installed `host.mjs`,
//! joined into the helper's [`CompanionControl`] and [`SidecarOps`].
//!
//! Nothing here needs the browser to be installed: the helper runs anyway
//! and opens its link with `ready: not_installed` or `failed` and the
//! reason ([`CompanionControl::readiness`]); the sidecar is found when it
//! first starts.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{json, Value};

use super::companion::{Companion, CompanionOptions, EnsureError};
use super::gate::GatedOp;
use super::helper::{CompanionControl, MachineParts, SidecarOps};
use super::sidecar::{
    NodeSidecar, Sidecar, SidecarOptions, SidecarProcess, SidecarSetupError, SidecarSpawn,
};
use crate::api::schema::{BrowserCompanionAppInfo, BrowserHostRegisterParams};
use crate::browser::companion_app::{ProcessSpawner, StartReason};
use crate::browser::host::HostReply;
use crate::browser::link_proto::{BrowserLinkReady, LinkError};
use crate::browser::BrowserError;

/// The raise a user's focus sends the companion.
const RAISE_DEADLINE: Duration = Duration::from_secs(3);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The installed sidecar, found when it first starts (and again while it
/// cannot be): a helper without one still runs and says why.
pub(crate) struct InstalledSidecar {
    browser_home: PathBuf,
    node: Option<String>,
    found: Mutex<Option<NodeSidecar>>,
}

impl InstalledSidecar {
    pub(crate) fn new(browser_home: PathBuf, node: Option<String>) -> Self {
        Self {
            browser_home,
            node,
            found: Mutex::new(None),
        }
    }

    fn find(&self) -> Result<NodeSidecar, SidecarSetupError> {
        let mut found = lock(&self.found);
        if let Some(sidecar) = found.as_ref() {
            return Ok(sidecar.clone());
        }
        let sidecar = NodeSidecar::resolve(&self.browser_home, self.node.as_deref())?;
        *found = Some(sidecar.clone());
        Ok(sidecar)
    }
}

impl SidecarSpawn for InstalledSidecar {
    fn spawn(&self) -> io::Result<SidecarProcess> {
        self.find()
            .map_err(|problem| io::Error::other(problem.detail()))?
            .spawn()
    }

    fn kill(&self, pid: u32) {
        crate::browser::host::kill_group(pid);
    }
}

/// The helper's view of the machine's companion and its sidecar.
pub(crate) struct MachineCompanion {
    companion: Companion,
    sidecar: Arc<Sidecar>,
    spawner: Arc<InstalledSidecar>,
    /// This Mac's name, for the remote side's texts.
    mac: String,
    /// The companion launch the link speaks for: the registered one's,
    /// else one made up until a companion registers.
    instance: Mutex<String>,
}

impl MachineCompanion {
    fn user_text(&self, error: &EnsureError) -> String {
        match error {
            EnsureError::NotInstalled => {
                "the companion browser is not installed; run \"herdr browser setup --companion\""
                    .into()
            }
            EnsureError::Failed(why) => why.clone(),
        }
    }
}

impl CompanionControl for MachineCompanion {
    fn recover(&self) {
        let recovered = self.companion.recover();
        tracing::info!(?recovered, "the machine's companion at the helper's start");
    }

    fn instance(&self) -> String {
        lock(&self.instance).clone()
    }

    fn readiness(&self) -> (BrowserLinkReady, Option<String>) {
        match self.companion.install_check() {
            Err(EnsureError::NotInstalled) => return (BrowserLinkReady::NotInstalled, None),
            Err(EnsureError::Failed(why)) => return (BrowserLinkReady::Failed, Some(why)),
            Ok(()) => {}
        }
        match self.spawner.find() {
            Ok(_) => (BrowserLinkReady::Ready, None),
            Err(problem) => (BrowserLinkReady::Failed, Some(problem.detail())),
        }
    }

    fn ensure(&self, wait: Duration) -> Result<(), LinkError> {
        if let Err(problem) = self.spawner.find() {
            return Err(EnsureError::Failed(problem.detail()).link_error(&self.mac));
        }
        self.companion
            .ensure(wait)
            .map_err(|error| error.link_error(&self.mac))
    }

    fn register(&self, params: &BrowserHostRegisterParams) -> Result<Value, BrowserError> {
        let answer = self.companion.register(params)?;
        *lock(&self.instance) = params.instance.clone();
        Ok(answer)
    }

    fn heartbeat(&self, launch_token: &str) -> bool {
        self.companion.heartbeat(launch_token)
    }

    fn unregistered(&self, launch_token: &str) {
        self.companion.unregistered(launch_token);
    }

    fn start(&self) -> Result<(), String> {
        self.companion
            .start(StartReason::User)
            .map_err(|error| self.user_text(&error))
    }

    fn stop(&self) {
        self.companion.stop();
    }

    fn restart(&self) -> Result<(), String> {
        self.companion
            .restart()
            .map_err(|error| self.user_text(&error))
    }

    fn focus(&self) -> Result<(), String> {
        if !self.companion.live() {
            return self.start();
        }
        let sidecar = Arc::clone(&self.sidecar);
        std::thread::Builder::new()
            .name("browser-link-raise".into())
            .spawn(move || {
                if let Err(err) =
                    sidecar.request("control_raise", None, json!({ "user": true }), RAISE_DEADLINE)
                {
                    tracing::warn!(code = %err.code, message = %err.message, "the machine's companion did not come forward");
                }
            })
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    fn forget(&self) -> io::Result<()> {
        self.companion.forget()
    }

    /// The companion only: the helper ends the sidecar after it.
    fn shutdown(&self) {
        self.companion.shutdown();
    }

    fn info(&self) -> BrowserCompanionAppInfo {
        self.companion.info()
    }
}

impl SidecarOps for Sidecar {
    fn op(&self, op: &GatedOp) -> Result<HostReply, BrowserError> {
        Sidecar::op(self, op)
    }

    fn shutdown(&self) {
        Sidecar::shutdown(self);
    }
}

/// The parts factory of `herdr browser-link`: the installed companion app
/// and sidecar under this Mac's browser home.
pub(crate) fn installed(
    parts: &MachineParts<'_>,
) -> io::Result<(Arc<dyn CompanionControl>, Arc<dyn SidecarOps>)> {
    let browser_home = crate::browser::browser_home();
    let config = crate::config::Config::load().config.browser;
    let node = Some(config.node.trim().to_owned()).filter(|node| !node.is_empty());
    let spawner = Arc::new(InstalledSidecar::new(browser_home.clone(), node));
    let sidecar = Arc::new(Sidecar::new(
        Arc::clone(&spawner) as Arc<dyn SidecarSpawn>,
        Arc::clone(&parts.hooks) as Arc<dyn super::sidecar::SidecarHooks>,
        SidecarOptions::default(),
    ));
    let companion = Companion::new(
        CompanionOptions {
            companion_home: crate::browser::companion_install::companion_home(&browser_home),
            key: parts.key.to_owned(),
            label: parts.label.to_owned(),
            api_socket: parts.api_socket.to_owned(),
        },
        Arc::new(ProcessSpawner),
        Arc::clone(&parts.proxy),
        Arc::clone(&sidecar) as Arc<dyn super::companion::CompanionLink>,
        Arc::clone(&parts.hooks) as Arc<dyn super::companion::CompanionHooks>,
    )?;
    let control = Arc::new(MachineCompanion {
        companion,
        sidecar: Arc::clone(&sidecar),
        spawner,
        mac: parts.mac.to_owned(),
        instance: Mutex::new(crate::browser::session::random_hex(16)),
    });
    Ok((control, sidecar))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_sidecar_is_a_reason_not_a_failed_helper() {
        let home = super::super::api_socket::tests::short_dir();
        let sidecar = InstalledSidecar::new(home.path().to_owned(), None);
        let err = sidecar.spawn().err().expect("nothing is installed");
        assert_eq!(err.to_string(), SidecarSetupError::Missing.detail());
        assert!(sidecar.find().is_err(), "it is looked for again");
    }
}
