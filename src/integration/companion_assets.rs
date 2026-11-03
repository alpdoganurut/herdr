//! The herdr+ companion browser's app files (fork): every runtime file of
//! `apps/companion` (its `package.json`, `src/**`, the eye-plus icon), embedded
//! in the binary and installed by `herdr browser setup --companion` into a
//! versioned directory under `state_dir()/browser/companion-app/app/` (see
//! `crate::browser::companion_install`). Tests, spikes, scripts and
//! `node_modules` are never embedded; Electron itself is installed by npm.

use sha2::{Digest, Sha256};

/// The app's version (`apps/companion/package.json`), checked by a test.
pub const COMPANION_APP_VERSION: &str = "0.2.0";

/// File name (relative to the app directory) and content, in write order.
pub const COMPANION_ASSETS: &[(&str, &[u8])] = &[
    (
        "package.json",
        include_bytes!("../../apps/companion/package.json"),
    ),
    (
        "src/groups.js",
        include_bytes!("../../apps/companion/src/groups.js"),
    ),
    (
        "src/herdr-api.js",
        include_bytes!("../../apps/companion/src/herdr-api.js"),
    ),
    (
        "src/host.js",
        include_bytes!("../../apps/companion/src/host.js"),
    ),
    (
        "src/main.js",
        include_bytes!("../../apps/companion/src/main.js"),
    ),
    (
        "src/managed.js",
        include_bytes!("../../apps/companion/src/managed.js"),
    ),
    (
        "src/views.js",
        include_bytes!("../../apps/companion/src/views.js"),
    ),
    (
        "src/preload/control.js",
        include_bytes!("../../apps/companion/src/preload/control.js"),
    ),
    (
        "src/preload/nonet.js",
        include_bytes!("../../apps/companion/src/preload/nonet.js"),
    ),
    (
        "src/preload/strip.js",
        include_bytes!("../../apps/companion/src/preload/strip.js"),
    ),
    (
        "src/ui/control.html",
        include_bytes!("../../apps/companion/src/ui/control.html"),
    ),
    (
        "src/ui/strip.css",
        include_bytes!("../../apps/companion/src/ui/strip.css"),
    ),
    (
        "src/ui/strip.html",
        include_bytes!("../../apps/companion/src/ui/strip.html"),
    ),
    (
        "src/ui/strip.js",
        include_bytes!("../../apps/companion/src/ui/strip.js"),
    ),
    (
        "src/ui/icons/allow.svg",
        include_bytes!("../../apps/companion/src/ui/icons/allow.svg"),
    ),
    (
        "src/ui/icons/attention.svg",
        include_bytes!("../../apps/companion/src/ui/icons/attention.svg"),
    ),
    (
        "src/ui/icons/back.svg",
        include_bytes!("../../apps/companion/src/ui/icons/back.svg"),
    ),
    (
        "src/ui/icons/close.svg",
        include_bytes!("../../apps/companion/src/ui/icons/close.svg"),
    ),
    (
        "src/ui/icons/deny.svg",
        include_bytes!("../../apps/companion/src/ui/icons/deny.svg"),
    ),
    (
        "src/ui/icons/eye-plus-small.svg",
        include_bytes!("../../apps/companion/src/ui/icons/eye-plus-small.svg"),
    ),
    (
        "src/ui/icons/forward.svg",
        include_bytes!("../../apps/companion/src/ui/icons/forward.svg"),
    ),
    (
        "src/ui/icons/newtab.svg",
        include_bytes!("../../apps/companion/src/ui/icons/newtab.svg"),
    ),
    (
        "src/ui/icons/reload.svg",
        include_bytes!("../../apps/companion/src/ui/icons/reload.svg"),
    ),
    (
        "src/ui/icons/send.svg",
        include_bytes!("../../apps/companion/src/ui/icons/send.svg"),
    ),
    (
        "src/ui/icons/stop.svg",
        include_bytes!("../../apps/companion/src/ui/icons/stop.svg"),
    ),
    (
        "assets/icon/eye-plus-512.png",
        include_bytes!("../../apps/companion/assets/icon/eye-plus-512.png"),
    ),
    (
        "assets/icon/eye-plus.icns",
        include_bytes!("../../apps/companion/assets/icon/eye-plus.icns"),
    ),
];

/// The app icon the branded bundle carries (`Contents/Resources/eye-plus.icns`).
pub const COMPANION_ICNS: &[u8] = include_bytes!("../../apps/companion/assets/icon/eye-plus.icns");

/// The Electron runtime's npm manifest and pinned lock (electron only), written
/// next to its `node_modules` before `npm ci`.
pub const ELECTRON_MANIFEST: &str = include_str!("../../apps/companion/install/package.json");
pub const ELECTRON_LOCK: &str = include_str!("../../apps/companion/install/package-lock.json");

/// SHA-256 over every embedded file (name and content), hex.
pub fn companion_app_sha256() -> String {
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = Sha256::new();
            for (name, content) in COMPANION_ASSETS {
                hasher.update(name.as_bytes());
                hasher.update([0]);
                hasher.update(content);
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

/// The first 12 hex characters of [`companion_app_sha256`]: the versioned
/// app directory's name.
pub fn companion_app_sha12() -> String {
    companion_app_sha256().chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn runtime_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
            .flatten()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                runtime_files(root, &path, out);
            } else if path
                .file_name()
                .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
            {
                out.push(
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }

    #[test]
    fn every_runtime_file_of_the_app_is_embedded() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/companion");
        let mut on_disk = Vec::new();
        runtime_files(&root, &root.join("src"), &mut on_disk);
        let embedded: Vec<&str> = COMPANION_ASSETS.iter().map(|(name, _)| *name).collect();
        for file in &on_disk {
            assert!(
                embedded.contains(&file.as_str()),
                "apps/companion/{file} is not embedded in COMPANION_ASSETS"
            );
        }
        for (name, content) in COMPANION_ASSETS {
            let disk = std::fs::read(root.join(name))
                .unwrap_or_else(|err| panic!("apps/companion/{name}: {err}"));
            assert!(
                disk == *content,
                "apps/companion/{name} differs from the embedded copy (rebuild)"
            );
            assert!(
                !name.starts_with("test/")
                    && !name.starts_with("spike/")
                    && !name.starts_with("scripts/")
                    && !name.contains("node_modules"),
                "{name} is not a runtime file"
            );
        }
        let names: std::collections::HashSet<&str> = embedded.iter().copied().collect();
        assert_eq!(names.len(), embedded.len(), "an asset is listed twice");
    }

    #[test]
    fn the_version_is_the_app_package_version_and_the_digest_is_stable() {
        let package: serde_json::Value = serde_json::from_slice(COMPANION_ASSETS[0].1).unwrap();
        assert_eq!(package["version"], COMPANION_APP_VERSION);
        assert_eq!(package["productName"], "herdr+ companion");
        let manifest: serde_json::Value = serde_json::from_str(ELECTRON_MANIFEST).unwrap();
        assert_eq!(
            manifest["dependencies"]["electron"],
            crate::browser::companion_install::ELECTRON_VERSION,
            "the runtime manifest pins the Electron the installer expects"
        );
        let lock: serde_json::Value = serde_json::from_str(ELECTRON_LOCK).unwrap();
        assert_eq!(
            lock["packages"]["node_modules/electron"]["version"],
            crate::browser::companion_install::ELECTRON_VERSION
        );
        assert_eq!(companion_app_sha256().len(), 64);
        assert_eq!(companion_app_sha12().len(), 12);
        assert_eq!(companion_app_sha256(), companion_app_sha256());
    }
}
