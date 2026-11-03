//! Installing the herdr+ companion browser (fork, companion launcher):
//! `herdr browser setup --companion` puts the app herdr runs under
//! `<browser home>/companion-app/`:
//!
//! - `app/<sha12>/`: the app files embedded in this herdr
//!   (`integration::companion_assets`), one directory per bundled version
//!   (written to a temporary directory and renamed); `current` names the
//!   active one. A running companion keeps its directory while a newer one
//!   is installed; older ones go (the newest two and the running one stay).
//! - `electron/<version>/`: the pinned npm manifest and lock (electron only),
//!   `node_modules/` from `npm ci --ignore-scripts`, then the Electron binary
//!   (`node node_modules/electron/install.js`) — a download of about 115 MB
//!   that only an explicit, consented setup runs; a later `npm ci` (a new
//!   lock) keeps the binary aside and puts it back. `HERDR_COMPANION_ELECTRON_DIST`
//!   clones an existing Electron `dist` directory instead (tests, offline).
//! - `bundle/<version>/herdr+ companion.app` (macOS): a clone of
//!   `Electron.app` named `herdr+ companion` with the eye-plus icon and its
//!   own bundle id, ad-hoc signed; a signing failure keeps the plain
//!   `Electron.app` (`branded: false`).
//! - `install.json`: what is installed; a rerun with nothing to do writes
//!   nothing. `profile/` (the app's cookies and logins), `run.json` and
//!   `companion.log` belong to the launcher (`companion_app`).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::setup::{self, SetupEnv};
use crate::api::schema::{BrowserCheckInfo, BrowserCompanionAppInfo, BrowserFixKind};
use crate::config::BrowserConfig;
use crate::integration::companion_assets::{
    companion_app_sha12, companion_app_sha256, COMPANION_APP_VERSION, COMPANION_ASSETS,
    COMPANION_ICNS, ELECTRON_LOCK, ELECTRON_MANIFEST,
};

/// The directory under the browser home.
pub const COMPANION_DIR: &str = "companion-app";
/// The Electron the app is built for (`apps/companion/install/package.json`).
pub const ELECTRON_VERSION: &str = "45.2.1";
/// The app's name on macOS (menu bar, Dock, the bundle's file name).
pub const APP_NAME: &str = "herdr+ companion";
/// The branded bundle's id.
pub const BUNDLE_ID: &str = "dev.herdr.plus.companion";
/// The icon file in the branded bundle's `Contents/Resources`.
pub const ICON_NAME: &str = "eye-plus";
pub const INSTALL_FILE: &str = "install.json";
pub const INSTALL_VERSION: u32 = 1;
/// Clone this Electron `dist` directory instead of npm and the download.
pub const ELECTRON_DIST_ENV: &str = "HERDR_COMPANION_ELECTRON_DIST";
/// What the Electron download costs, in the words every message uses.
pub const DOWNLOAD_SIZE: &str = "about 115 MB";
/// App directories kept: the current one and the newest older one (a
/// running companion's is kept whatever its age).
const KEEP_APP_DIRS: usize = 2;

pub fn companion_home(browser_home: &Path) -> PathBuf {
    browser_home.join(COMPANION_DIR)
}

pub fn app_root(home: &Path) -> PathBuf {
    home.join("app")
}

pub fn app_dir(home: &Path, sha12: &str) -> PathBuf {
    app_root(home).join(sha12)
}

pub fn electron_dir(home: &Path) -> PathBuf {
    home.join("electron").join(ELECTRON_VERSION)
}

/// Where npm (or the clone) puts Electron's binary distribution.
pub fn electron_dist(home: &Path) -> PathBuf {
    electron_dir(home)
        .join("node_modules")
        .join("electron")
        .join("dist")
}

pub fn branded_bundle(home: &Path) -> PathBuf {
    home.join("bundle")
        .join(ELECTRON_VERSION)
        .join(format!("{APP_NAME}.app"))
}

pub fn profile_dir(home: &Path) -> PathBuf {
    home.join("profile")
}

pub fn log_path(home: &Path) -> PathBuf {
    home.join("companion.log")
}

/// The unbranded bundle (macOS) or the dist directory (elsewhere).
pub fn plain_bundle(home: &Path) -> PathBuf {
    let dist = electron_dist(home);
    if cfg!(target_os = "macos") {
        dist.join("Electron.app")
    } else {
        dist
    }
}

/// The executable inside a bundle (macOS) or a dist directory (elsewhere).
/// The inner binary keeps its name: Electron finds its helpers by it.
pub fn bundle_executable(bundle: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        bundle.join("Contents").join("MacOS").join("Electron")
    } else if cfg!(windows) {
        bundle.join("electron.exe")
    } else {
        bundle.join("electron")
    }
}

/// `install.json`: what `setup --companion` installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InstallRecord {
    #[serde(default)]
    pub version: u32,
    /// The app version (`package.json`).
    #[serde(default)]
    pub app: String,
    /// The installed app files' digest (`companion_app_sha256`).
    #[serde(default)]
    pub app_sha256: String,
    #[serde(default)]
    pub electron: String,
    /// The bundle the launcher runs.
    #[serde(default)]
    pub bundle: String,
    #[serde(default)]
    pub branded: bool,
    /// Why the bundle is not branded (a signing failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unbranded: Option<String>,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub npm: String,
    #[serde(default)]
    pub installed_at: u64,
}

impl InstallRecord {
    /// The app directory (`app/<sha12>`) of the installed files.
    pub fn app_dir(&self, home: &Path) -> PathBuf {
        app_dir(home, &self.app_sha256.chars().take(12).collect::<String>())
    }

    fn same_install(&self, other: &Self) -> bool {
        self.version == other.version
            && self.app == other.app
            && self.app_sha256 == other.app_sha256
            && self.electron == other.electron
            && self.bundle == other.bundle
            && self.branded == other.branded
            && self.unbranded == other.unbranded
    }
}

pub fn read_install(home: &Path) -> Option<InstallRecord> {
    let text = std::fs::read_to_string(home.join(INSTALL_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_install(home: &Path, record: &InstallRecord) -> io::Result<()> {
    let json = serde_json::to_string_pretty(record).map_err(io::Error::other)?;
    write_atomic(&home.join(INSTALL_FILE), json.as_bytes())
}

/// The installed Electron binary's version (`dist/version`), when there is one.
pub fn electron_installed(home: &Path) -> Option<String> {
    let version = std::fs::read_to_string(electron_dist(home).join("version")).ok()?;
    let version = version.trim().trim_start_matches('v').to_string();
    (!version.is_empty() && bundle_executable(&plain_bundle(home)).is_file()).then_some(version)
}

/// The version of the npm package in `node_modules/electron`.
fn electron_package_version(home: &Path) -> Option<String> {
    let path = electron_dir(home)
        .join("node_modules")
        .join("electron")
        .join("package.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    value["version"].as_str().map(str::to_string)
}

/// What the launcher runs: the app directory and the executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionInstall {
    pub record: InstallRecord,
    pub app_dir: PathBuf,
    pub executable: PathBuf,
}

/// Why the launcher cannot run the companion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallProblem {
    NotInstalled,
    NoElectron,
    Unsupported(String),
}

/// The install the launcher runs, or why there is none.
pub fn resolve(home: &Path) -> Result<CompanionInstall, InstallProblem> {
    if cfg!(windows) {
        return Err(InstallProblem::Unsupported(
            "the companion browser is not supported on Windows yet".into(),
        ));
    }
    let record = read_install(home).ok_or(InstallProblem::NotInstalled)?;
    let app_dir = record.app_dir(home);
    if !app_dir.join("package.json").is_file() {
        return Err(InstallProblem::NotInstalled);
    }
    let bundle = PathBuf::from(&record.bundle);
    let executable = bundle_executable(&bundle);
    if record.bundle.is_empty() || !executable.is_file() {
        return Err(InstallProblem::NoElectron);
    }
    Ok(CompanionInstall {
        record,
        app_dir,
        executable,
    })
}

// ---------------------------------------------------------------------------
// App files

/// The outcome of writing the app files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFiles {
    pub sha12: String,
    pub dir: PathBuf,
    /// The directory was written now (else it was already there).
    pub written: bool,
}

/// Write the bundled app files into `app/<sha12>/` unless they are there,
/// point `current` at them, prune old directories (never `keep`, the one a
/// running companion uses).
pub fn install_app_files(home: &Path, keep: Option<&Path>) -> io::Result<AppFiles> {
    let sha12 = companion_app_sha12();
    let root = app_root(home);
    let dir = app_dir(home, &sha12);
    let written = if dir.join("package.json").is_file() {
        false
    } else {
        std::fs::create_dir_all(&root)?;
        let tmp = root.join(format!(".{sha12}.tmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let result = (|| -> io::Result<()> {
            for (name, content) in COMPANION_ASSETS {
                let path = tmp.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content)?;
            }
            match std::fs::rename(&tmp, &dir) {
                // another setup won the race: the same files are in place
                Err(_) if dir.join("package.json").is_file() => {
                    let _ = std::fs::remove_dir_all(&tmp);
                    Ok(())
                }
                other => other,
            }
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        result?;
        true
    };
    let current = root.join("current");
    if std::fs::read_to_string(&current)
        .ok()
        .as_deref()
        .map(str::trim)
        != Some(sha12.as_str())
    {
        write_atomic(&current, format!("{sha12}\n").as_bytes())?;
    }
    prune_app_dirs(&root, &sha12, keep);
    Ok(AppFiles {
        sha12,
        dir,
        written,
    })
}

/// Keep `current`, the newest older directory and `keep`; remove the rest.
fn prune_app_dirs(root: &Path, current: &str, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
        })
        .map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (modified, path)
        })
        .collect();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.0));
    let mut kept = 0;
    for (_, path) in dirs {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let running = keep.is_some_and(|keep| keep == path);
        if name == current || running {
            kept += usize::from(name == current);
            continue;
        }
        if kept < KEEP_APP_DIRS {
            kept += 1;
            continue;
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// The installed app files differ from this herdr's: rewrite them and the
/// record (no download, no re-branding). `None` when the companion is not
/// installed or already current. Plain `setup` and the server's
/// auto-repair after a herdr update run this.
pub fn refresh_app_files(home: &Path, keep: Option<&Path>) -> Option<Result<String, String>> {
    let mut record = read_install(home)?;
    if record.app_sha256 == companion_app_sha256() && record.app_dir(home).is_dir() {
        return None;
    }
    let outcome = install_app_files(home, keep)
        .map_err(|err| format!("companion: cannot write the app files ({err})"))
        .and_then(|files| {
            let before = record.app.clone();
            record.app = COMPANION_APP_VERSION.into();
            record.app_sha256 = companion_app_sha256();
            write_install(home, &record)
                .map_err(|err| format!("companion: cannot write {INSTALL_FILE} ({err})"))?;
            Ok(format!(
                "companion: app files {before} → {COMPANION_APP_VERSION} ({})",
                files.sha12
            ))
        });
    Some(outcome)
}

// ---------------------------------------------------------------------------
// Setup

/// How `setup --companion` may proceed.
#[derive(Debug, Clone, Default)]
pub struct CompanionSetupOptions {
    /// The user agreed to the Electron download (`--yes`, or the prompt).
    pub consent: bool,
    /// Clone this Electron `dist` directory instead of npm and the download.
    pub dist_override: Option<PathBuf>,
    /// Brand and ad-hoc sign the macOS bundle (tests turn it off).
    pub brand: bool,
}

impl CompanionSetupOptions {
    /// The options the CLI and the settings fix use: `HERDR_COMPANION_ELECTRON_DIST`
    /// when set, branding on.
    pub fn from_process(consent: bool) -> Self {
        Self {
            consent,
            dist_override: std::env::var_os(ELECTRON_DIST_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            brand: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompanionSetupError {
    /// The Electron binary is missing and the download was not agreed to.
    NeedsConsent,
    Unsupported(String),
    Failed(String),
}

impl std::fmt::Display for CompanionSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeedsConsent => write!(
                f,
                "companion: Electron not downloaded — run \"herdr browser setup --companion\" in a terminal (or add --yes)"
            ),
            Self::Unsupported(why) | Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// What a setup did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionSetupReport {
    pub record: InstallRecord,
    /// Something was written.
    pub changed: bool,
    /// One line per step that did something, then the summary.
    pub lines: Vec<String>,
}

/// The consent question (the CLI asks it; `--yes` answers it).
pub fn download_question() -> String {
    format!(
        "Download Electron {ELECTRON_VERSION} for {} ({DOWNLOAD_SIZE}; skipped when the Electron cache already has it)? [y/N] ",
        platform_label()
    )
}

fn platform_label() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// `herdr browser setup --companion`: app files, Electron, the macOS bundle,
/// `install.json`. One setup at a time (the browser home's setup lock).
pub fn setup_companion(
    env: &SetupEnv,
    config: &BrowserConfig,
    options: &CompanionSetupOptions,
) -> Result<CompanionSetupReport, CompanionSetupError> {
    if cfg!(windows) {
        return Err(CompanionSetupError::Unsupported(
            "companion: not supported on Windows yet".into(),
        ));
    }
    let home = companion_home(&env.browser_home);
    std::fs::create_dir_all(&home).map_err(|err| {
        CompanionSetupError::Failed(format!(
            "companion: cannot create {} ({err})",
            home.display()
        ))
    })?;
    let _lock = setup::helper_lock(&home).map_err(CompanionSetupError::Failed)?;
    let previous = read_install(&home);
    let running_app = super::companion_app::running_app_dir(&home);
    let mut lines = Vec::new();

    // 1. the app files
    let files = install_app_files(&home, running_app.as_deref()).map_err(|err| {
        CompanionSetupError::Failed(format!("companion: cannot write the app files ({err})"))
    })?;
    if files.written {
        lines.push(format!(
            "companion: app files {COMPANION_APP_VERSION} written ({})",
            files.sha12
        ));
    }

    // 2. Electron
    let (node, npm) = install_electron(env, config, &home, options, &mut lines)?;

    // 3. the bundle
    let (bundle, branded, unbranded) = if cfg!(target_os = "macos") && options.brand {
        match brand(&home, &mut lines) {
            Ok(bundle) => (bundle, true, None),
            Err(why) => {
                lines.push(format!("companion: running unbranded (codesign: {why})"));
                (plain_bundle(&home), false, Some(why))
            }
        }
    } else {
        (plain_bundle(&home), false, None)
    };

    // 4. the record
    let mut record = InstallRecord {
        version: INSTALL_VERSION,
        app: COMPANION_APP_VERSION.into(),
        app_sha256: companion_app_sha256(),
        electron: ELECTRON_VERSION.into(),
        bundle: bundle.display().to_string(),
        branded,
        unbranded,
        node,
        npm,
        installed_at: super::unix_now(),
    };
    let changed = match &previous {
        Some(previous) if previous.same_install(&record) => {
            record.installed_at = previous.installed_at;
            record.node = previous.node.clone();
            record.npm = previous.npm.clone();
            !lines.is_empty()
        }
        _ => {
            write_install(&home, &record).map_err(|err| {
                CompanionSetupError::Failed(format!(
                    "companion: cannot write {INSTALL_FILE} ({err})"
                ))
            })?;
            true
        }
    };
    if changed {
        lines.push(format!(
            "companion: app {COMPANION_APP_VERSION} ({}) · electron {ELECTRON_VERSION} · {}",
            files.sha12,
            bundle
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
    } else {
        lines.push("companion: up to date".into());
    }
    Ok(CompanionSetupReport {
        record,
        changed,
        lines,
    })
}

/// The npm package and the binary; answers the node and npm used (empty
/// when the dist was cloned).
fn install_electron(
    env: &SetupEnv,
    config: &BrowserConfig,
    home: &Path,
    options: &CompanionSetupOptions,
    lines: &mut Vec<String>,
) -> Result<(String, String), CompanionSetupError> {
    let failed = CompanionSetupError::Failed;
    let dir = electron_dir(home);
    if let Some(source) = options.dist_override.as_deref() {
        if electron_installed(home).as_deref() == Some(ELECTRON_VERSION) {
            return Ok((String::new(), String::new()));
        }
        let version = std::fs::read_to_string(source.join("version"))
            .map(|v| v.trim().trim_start_matches('v').to_string())
            .map_err(|err| {
                failed(format!(
                    "companion: {ELECTRON_DIST_ENV}={} has no version file ({err})",
                    source.display()
                ))
            })?;
        if version != ELECTRON_VERSION {
            return Err(failed(format!(
                "companion: {ELECTRON_DIST_ENV}={} is Electron {version}; this herdr needs {ELECTRON_VERSION}",
                source.display()
            )));
        }
        let package_dir = dir.join("node_modules").join("electron");
        std::fs::create_dir_all(&package_dir).map_err(|err| {
            failed(format!(
                "companion: cannot create {} ({err})",
                package_dir.display()
            ))
        })?;
        write_atomic(
            &package_dir.join("package.json"),
            format!("{{\"name\":\"electron\",\"version\":\"{ELECTRON_VERSION}\"}}\n").as_bytes(),
        )
        .map_err(|err| failed(format!("companion: {err}")))?;
        let dist = electron_dist(home);
        let _ = std::fs::remove_dir_all(&dist);
        clone_tree(source, &dist).map_err(|err| {
            failed(format!(
                "companion: cannot copy {} ({err})",
                source.display()
            ))
        })?;
        lines.push(format!(
            "companion: electron {ELECTRON_VERSION} copied from {}",
            source.display()
        ));
        return Ok((String::new(), String::new()));
    }

    // a setup cut short while the binary waited outside node_modules (below)
    let _ = restore_kept_dist(home);
    // npm ci of the pinned manifest (electron only), unless it is in place
    let manifest_changed = std::fs::read_to_string(dir.join("package-lock.json"))
        .ok()
        .as_deref()
        != Some(ELECTRON_LOCK);
    let need_npm =
        manifest_changed || electron_package_version(home).as_deref() != Some(ELECTRON_VERSION);
    let need_binary = electron_installed(home).as_deref() != Some(ELECTRON_VERSION);
    if !need_npm && !need_binary {
        return Ok((String::new(), String::new()));
    }
    // asked before anything touches the network (npm included) or the files
    if need_binary && !options.consent {
        return Err(CompanionSetupError::NeedsConsent);
    }
    let node = super::node::discover_default(env.node_override.as_deref().or(config.node()), None)
        .ok_or_else(|| {
            failed(
            "companion: node not found; install Node.js 22+ (nvm, Homebrew) or set [browser] node"
                .into(),
        )
        })?;
    let npm = super::node::npm_beside(&node.path).or_else(|| setup::on_path("npm"));
    let node_text = node.path.display().to_string();
    let npm_text = npm
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let path_env = {
        let mut paths = vec![node
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        std::env::join_paths(paths).unwrap_or_default()
    };
    if need_npm {
        std::fs::create_dir_all(&dir).map_err(|err| {
            failed(format!(
                "companion: cannot create {} ({err})",
                dir.display()
            ))
        })?;
        write_atomic(&dir.join("package.json"), ELECTRON_MANIFEST.as_bytes())
            .and_then(|()| write_atomic(&dir.join("package-lock.json"), ELECTRON_LOCK.as_bytes()))
            .map_err(|err| failed(format!("companion: {err}")))?;
        let Some(npm) = npm.as_deref() else {
            return Err(failed(format!(
                "companion: npm not found beside {} or on PATH",
                node.path.display()
            )));
        };
        // npm ci empties node_modules, and Electron's binary (electron/dist)
        // with it: this Electron's binary, already here, waits outside and
        // comes back (a lock change never costs the download, nor consent)
        let keep = !need_binary;
        if keep {
            keep_dist_aside(home).map_err(|err| {
                failed(format!(
                    "companion: cannot keep the Electron binary across npm ci ({err})"
                ))
            })?;
        }
        let ran = std::process::Command::new(npm)
            .args([
                "ci",
                "--ignore-scripts",
                "--omit=dev",
                "--no-audit",
                "--no-fund",
            ])
            .current_dir(&dir)
            .env("PATH", &path_env)
            .stdin(std::process::Stdio::null())
            .output();
        if keep {
            restore_kept_dist(home).map_err(|err| {
                failed(format!(
                    "companion: cannot put the Electron binary back after npm ci ({err}); it is in {}",
                    kept_dist(home).display()
                ))
            })?;
        }
        let output =
            ran.map_err(|err| failed(format!("companion: cannot run {} ({err})", npm.display())))?;
        if !output.status.success() {
            return Err(failed(format!(
                "companion: npm ci failed ({}) in {}: {}",
                output.status,
                dir.display(),
                last_lines(&String::from_utf8_lossy(&output.stderr), 4).join(" | ")
            )));
        }
        lines.push(format!("companion: npm ci (electron {ELECTRON_VERSION})"));
    }
    if electron_installed(home).as_deref() != Some(ELECTRON_VERSION) {
        if !options.consent {
            return Err(CompanionSetupError::NeedsConsent);
        }
        let output = std::process::Command::new(&node.path)
            .arg(dir.join("node_modules").join("electron").join("install.js"))
            .current_dir(&dir)
            .env("PATH", &path_env)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|err| failed(format!("companion: cannot run node ({err})")))?;
        if !output.status.success() || electron_installed(home).as_deref() != Some(ELECTRON_VERSION)
        {
            return Err(failed(format!(
                "companion: the Electron download failed ({}): {}",
                output.status,
                last_lines(&String::from_utf8_lossy(&output.stderr), 4).join(" | ")
            )));
        }
        lines.push(format!("companion: electron {ELECTRON_VERSION} downloaded"));
    }
    Ok((node_text, npm_text))
}

/// Where the Electron binary waits while `npm ci` empties `node_modules`.
fn kept_dist(home: &Path) -> PathBuf {
    electron_dir(home).join(".dist-kept")
}

fn keep_dist_aside(home: &Path) -> io::Result<()> {
    let kept = kept_dist(home);
    let _ = std::fs::remove_dir_all(&kept);
    std::fs::rename(electron_dist(home), &kept)
}

/// Put a binary kept aside back (also one a setup cut short left there),
/// unless this Electron's binary is in place already.
fn restore_kept_dist(home: &Path) -> io::Result<()> {
    let kept = kept_dist(home);
    if !kept.exists() {
        return Ok(());
    }
    if electron_installed(home).as_deref() == Some(ELECTRON_VERSION) {
        return std::fs::remove_dir_all(&kept);
    }
    let dist = electron_dist(home);
    if dist.exists() {
        std::fs::remove_dir_all(&dist)?;
    }
    if let Some(parent) = dist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&kept, &dist)
}

fn last_lines(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// Copy a directory tree: an APFS clone where it can (`cp -c`), else a plain copy.
fn clone_tree(source: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let src = source.display().to_string();
    let dst = dest.display().to_string();
    let attempts: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["-c", "-R"], &["-R"]]
    } else {
        &[&["-a", "--reflink=auto"], &["-a"]]
    };
    let mut last = String::new();
    for flags in attempts {
        let _ = std::fs::remove_dir_all(dest);
        let output = std::process::Command::new("cp")
            .args(*flags)
            .arg(&src)
            .arg(&dst)
            .stdin(std::process::Stdio::null())
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        last = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    Err(io::Error::other(last))
}

/// The branded bundle (macOS): made from `Electron.app` when it is missing
/// or stale, then ad-hoc signed and verified. Answers its path, or why the
/// plain bundle is used.
fn brand(home: &Path, lines: &mut Vec<String>) -> Result<PathBuf, String> {
    let target = branded_bundle(home);
    let plist = target.join("Contents").join("Info.plist");
    let current = target
        .join("Contents")
        .join("Resources")
        .join(format!("{ICON_NAME}.icns"));
    let stamp = target.join("Contents").join("herdr-companion.json");
    let wanted_stamp =
        format!("{{\"electron\":\"{ELECTRON_VERSION}\",\"bundle_id\":\"{BUNDLE_ID}\"}}\n");
    if plist.is_file()
        && current.is_file()
        && std::fs::read_to_string(&stamp).ok().as_deref() == Some(wanted_stamp.as_str())
        && bundle_executable(&target).is_file()
    {
        return Ok(target);
    }
    let source = plain_bundle(home);
    if !source.join("Contents").join("Info.plist").is_file() {
        return Err(format!("{} is not an app bundle", source.display()));
    }
    let parent = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.to_path_buf());
    let tmp = parent.join(format!(".{APP_NAME}.app.tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let result = (|| -> Result<(), String> {
        clone_tree(&source, &tmp).map_err(|err| format!("copy: {err}"))?;
        let tmp_plist = tmp.join("Contents").join("Info.plist");
        super::brand::brand_bundle(&tmp, APP_NAME)?;
        super::brand::plist_set(&tmp_plist, "CFBundleIdentifier", BUNDLE_ID)?;
        super::brand::plist_set(&tmp_plist, "CFBundleIconFile", ICON_NAME)?;
        std::fs::write(
            tmp.join("Contents")
                .join("Resources")
                .join(format!("{ICON_NAME}.icns")),
            COMPANION_ICNS,
        )
        .map_err(|err| format!("icon: {err}"))?;
        let tmp_text = tmp.display().to_string();
        let _ = setup::run_capture(
            Path::new("/usr/bin/xattr"),
            &["-dr", "com.apple.quarantine", &tmp_text],
            None,
        );
        setup::run_capture(
            Path::new("/usr/bin/codesign"),
            &["--force", "--deep", "--sign", "-", &tmp_text],
            None,
        )?;
        setup::run_capture(
            Path::new("/usr/bin/codesign"),
            &["--verify", "--deep", "--strict", &tmp_text],
            None,
        )?;
        std::fs::write(
            tmp.join("Contents").join("herdr-companion.json"),
            &wanted_stamp,
        )
        .map_err(|err| format!("stamp: {err}"))?;
        let old = parent.join(format!(".{APP_NAME}.app.old-{}", std::process::id()));
        if target.exists() {
            std::fs::rename(&target, &old).map_err(|err| format!("move the old bundle: {err}"))?;
        }
        std::fs::rename(&tmp, &target).map_err(|err| format!("install the bundle: {err}"))?;
        let _ = std::fs::remove_dir_all(&old);
        Ok(())
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(err);
    }
    lines.push(format!(
        "companion: {APP_NAME}.app branded and ad-hoc signed ({})",
        target.display()
    ));
    Ok(target)
}

// ---------------------------------------------------------------------------
// The doctor's check

fn check(ok: bool, detail: impl Into<String>, fix: BrowserFixKind) -> BrowserCheckInfo {
    BrowserCheckInfo {
        id: "companion_app".into(),
        ok,
        detail: detail.into(),
        fixable: fix != BrowserFixKind::None,
        fix_kind: fix,
        waits_for_agents: 0,
    }
}

/// The `companion_app` check: what is installed, and how the managed
/// process does (`live`, the server's picture).
pub fn doctor_check(home: &Path, live: Option<&BrowserCompanionAppInfo>) -> BrowserCheckInfo {
    if cfg!(windows) {
        return check(
            false,
            "not supported on Windows yet — set [browser] window = true for the herdr+ window",
            BrowserFixKind::None,
        );
    }
    // Not fixable from the settings overlay: the Electron download asks in a
    // terminal (and the Browser row says "companion not installed" itself).
    let Some(record) = read_install(home) else {
        return check(
            false,
            format!("not installed — herdr browser setup --companion (downloads Electron, {DOWNLOAD_SIZE})"),
            BrowserFixKind::None,
        );
    };
    if record.bundle.is_empty() || !bundle_executable(Path::new(&record.bundle)).is_file() {
        return check(
            false,
            "Electron not downloaded — herdr browser setup --companion",
            BrowserFixKind::None,
        );
    }
    if record.app_sha256 != companion_app_sha256() || !record.app_dir(home).is_dir() {
        return check(
            false,
            format!(
                "app files {} installed, {COMPANION_APP_VERSION} bundled — herdr browser setup",
                if record.app.is_empty() {
                    "?"
                } else {
                    &record.app
                }
            ),
            BrowserFixKind::Safe,
        );
    }
    if let Some(live) = live.filter(|live| live.state == "failed") {
        return check(
            false,
            format!(
                "failed: {} (last: {}) — herdr browser companion restart",
                crate::browser::companion_app::GIVE_UP_TEXT,
                live.last_error.as_deref().unwrap_or("unknown")
            ),
            BrowserFixKind::None,
        );
    }
    if cfg!(target_os = "macos") && !record.branded {
        return check(
            false,
            format!(
                "bundle not signed ({}) — herdr browser setup --companion",
                record.unbranded.as_deref().unwrap_or("plain Electron.app")
            ),
            BrowserFixKind::EditsFiles,
        );
    }
    let bundle = Path::new(&record.bundle)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let running = match live {
        Some(live) if live.state == "running" && live.managed => format!(
            " · running pid {} (managed)",
            live.pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into())
        ),
        Some(live) if live.state == "external" => " · running (started by hand)".to_string(),
        Some(live) if !live.state.is_empty() && live.state != "not_installed" => {
            format!(" · {}", live.state)
        }
        _ => String::new(),
    };
    check(
        true,
        format!(
            "app {} · electron {} · {bundle}{running}",
            record.app, record.electron
        ),
        BrowserFixKind::None,
    )
}

#[cfg(test)]
mod tests;
