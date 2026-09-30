//! The bundled browser sidecar (fork): `host.mjs` (the Playwright driver),
//! `extract.mjs` (main-content → markdown), `activity.mjs` (the activity
//! overlay and the companion driver), `package.json` + `package-lock.json`
//! (playwright-core pinned), `smoke.mjs` (a live check) and the companion
//! extension (`companion/`: tab groups), embedded in the binary and installed into
//! `state_dir()/browser/host/` by `herdr browser setup` (which then runs
//! `npm ci`) and refreshed by the server when only the scripts changed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::api::schema::BrowserRuntimeInfo;

/// The directory under the browser home the sidecar lives in.
pub const HOST_DIR: &str = "host";
/// The sidecar entry point.
pub const ENTRY: &str = "host.mjs";
/// The companion extension directory under `HOST_DIR` (`--load-extension`).
pub const COMPANION_DIR: &str = "companion";
pub const RUNTIME_FILE: &str = "runtime.json";
pub const RUNTIME_VERSION: u32 = 1;
/// The playwright-core version `package.json` pins.
pub const PLAYWRIGHT_CORE_VERSION: &str = "1.63.0";

/// File name and content, in the order they are written.
pub const BROWSER_ASSETS: &[(&str, &str)] = &[
    (ENTRY, include_str!("assets/browser/host.mjs")),
    ("extract.mjs", include_str!("assets/browser/extract.mjs")),
    ("activity.mjs", include_str!("assets/browser/activity.mjs")),
    ("package.json", include_str!("assets/browser/package.json")),
    (
        "package-lock.json",
        include_str!("assets/browser/package-lock.json"),
    ),
    ("smoke.mjs", include_str!("assets/browser/smoke.mjs")),
    (
        "companion/manifest.json",
        include_str!("assets/browser/companion/manifest.json"),
    ),
    (
        "companion/sw.js",
        include_str!("assets/browser/companion/sw.js"),
    ),
];

/// SHA-256 over every embedded asset (name and content), hex.
pub fn assets_sha256() -> String {
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = Sha256::new();
            for (name, content) in BROWSER_ASSETS {
                hasher.update(name.as_bytes());
                hasher.update([0]);
                hasher.update(content.as_bytes());
                hasher.update([0]);
            }
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        })
        .clone()
}

pub fn runtime_path(host_dir: &Path) -> PathBuf {
    host_dir.join(RUNTIME_FILE)
}

pub fn read_runtime(host_dir: &Path) -> Option<BrowserRuntimeInfo> {
    let content = fs::read_to_string(runtime_path(host_dir)).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_runtime(host_dir: &Path, runtime: &BrowserRuntimeInfo) -> io::Result<()> {
    fs::create_dir_all(host_dir)?;
    let path = runtime_path(host_dir);
    let tmp = host_dir.join(format!(".{RUNTIME_FILE}.tmp-{}", std::process::id()));
    let json = serde_json::to_string_pretty(runtime).map_err(io::Error::other)?;
    let result = fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Install the assets under `host_dir`, writing only what is missing or
/// stale. Returns how many files were written. `node_modules` is untouched
/// (that is `npm ci`'s job, run by `herdr browser setup`).
pub fn install(host_dir: &Path) -> io::Result<usize> {
    fs::create_dir_all(host_dir)?;
    let mut written = 0;
    for (name, content) in BROWSER_ASSETS {
        let path = host_dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let current = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        if current.as_deref() == Some(content.as_bytes()) {
            continue;
        }
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        let tmp = path.with_file_name(format!(".{file}.tmp-{}", std::process::id()));
        let result = fs::write(&tmp, content).and_then(|()| fs::rename(&tmp, &path));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        written += 1;
    }
    if written > 0 {
        tracing::info!(
            event = "browser.assets.install",
            dir = %host_dir.display(),
            written,
            "browser sidecar assets installed"
        );
    }
    Ok(written)
}

/// Whether `npm ci` has produced the pinned playwright-core.
pub fn playwright_installed(host_dir: &Path) -> Option<String> {
    let package = host_dir
        .join("node_modules")
        .join("playwright-core")
        .join("package.json");
    let content = fs::read_to_string(package).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-browser-assets-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn assets_are_the_bundled_files_pinned_to_one_playwright() {
        let names: Vec<&str> = BROWSER_ASSETS.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "host.mjs",
                "extract.mjs",
                "activity.mjs",
                "package.json",
                "package-lock.json",
                "smoke.mjs",
                "companion/manifest.json",
                "companion/sw.js"
            ]
        );
        let manifest: serde_json::Value = serde_json::from_str(
            BROWSER_ASSETS
                .iter()
                .find(|(n, _)| *n == "companion/manifest.json")
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(manifest["manifest_version"], 3);
        assert_eq!(
            manifest["permissions"],
            serde_json::json!(["tabs", "tabGroups", "alarms"])
        );
        assert_eq!(manifest["background"]["service_worker"], "sw.js");
        // The worker's VERSION and the sidecar's COMPANION_VERSION move together.
        let sw = BROWSER_ASSETS
            .iter()
            .find(|(n, _)| *n == "companion/sw.js")
            .unwrap()
            .1;
        let activity = BROWSER_ASSETS
            .iter()
            .find(|(n, _)| *n == "activity.mjs")
            .unwrap()
            .1;
        let after = |text: &str, marker: &str| -> u32 {
            let rest = &text[text.find(marker).expect(marker) + marker.len()..];
            rest.trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        assert_eq!(
            after(sw, "const VERSION ="),
            after(activity, "export const COMPANION_VERSION ="),
            "bump VERSION in companion/sw.js and COMPANION_VERSION in activity.mjs together"
        );
        let package: serde_json::Value = serde_json::from_str(
            BROWSER_ASSETS
                .iter()
                .find(|(n, _)| *n == "package.json")
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(
            package["dependencies"]["playwright-core"],
            PLAYWRIGHT_CORE_VERSION
        );
        let lock: serde_json::Value = serde_json::from_str(
            BROWSER_ASSETS
                .iter()
                .find(|(n, _)| *n == "package-lock.json")
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(
            lock["packages"]["node_modules/playwright-core"]["version"],
            PLAYWRIGHT_CORE_VERSION
        );
        assert!(
            lock["packages"]["node_modules/playwright-core"]["integrity"]
                .as_str()
                .is_some_and(|s| s.starts_with("sha512-"))
        );
        let host = BROWSER_ASSETS[0].1;
        assert!(host.contains("connectOverCDP"));
        assert!(host.contains("noDefaults: true"));
        assert!(host.contains("host_protocol"));
        assert_eq!(assets_sha256().len(), 64);
    }

    #[test]
    fn install_writes_missing_then_only_stale_and_runtime_round_trips() {
        let dir = temp_dir("install");
        assert_eq!(install(&dir).unwrap(), BROWSER_ASSETS.len());
        assert_eq!(install(&dir).unwrap(), 0);
        fs::write(dir.join("extract.mjs"), "edited").unwrap();
        assert_eq!(install(&dir).unwrap(), 1);
        assert!(dir.join(COMPANION_DIR).join("sw.js").is_file());
        assert!(dir.join(COMPANION_DIR).join("manifest.json").is_file());
        assert!(read_runtime(&dir).is_none());
        let runtime = BrowserRuntimeInfo {
            version: RUNTIME_VERSION,
            node: "/usr/bin/node".into(),
            node_version: Some("v22.14.0".into()),
            npm: None,
            playwright_core: Some(PLAYWRIGHT_CORE_VERSION.into()),
            assets_sha256: assets_sha256(),
            installed_at: 1,
        };
        write_runtime(&dir, &runtime).unwrap();
        assert_eq!(read_runtime(&dir), Some(runtime));
        assert!(playwright_installed(&dir).is_none());
        fs::create_dir_all(dir.join("node_modules/playwright-core")).unwrap();
        fs::write(
            dir.join("node_modules/playwright-core/package.json"),
            r#"{"version":"1.63.0"}"#,
        )
        .unwrap();
        assert_eq!(playwright_installed(&dir).as_deref(), Some("1.63.0"));
        let _ = fs::remove_dir_all(&dir);
    }
}
