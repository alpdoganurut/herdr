//! Browser profiles: one Chromium user data dir each under
//! `state_dir()/browser/profiles/<name>/`, listed in `profiles.json`
//! (name, creation time, temporary flag). Plain path operations, tested with
//! temporary directories.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::BrowserError;
use crate::config::valid_profile_name;

pub const PROFILES_DIR: &str = "profiles";
pub const PROFILES_FILE: &str = "profiles.json";
/// Written after the first successful launch; its presence turns on
/// `--restore-last-session` for the next one.
pub const LAUNCHED_MARKER: &str = ".herdr-launched";
/// `--profile new` creates `tmp-<stamp>`.
pub const TEMPORARY_PREFIX: &str = "tmp-";
const FILE_VERSION: u32 = 1;
/// Avatar index seeded into a new profile (Chromium's generic set).
const SEED_AVATAR_INDEX: u32 = 26;
/// Theme colour seeded into a new profile (ARGB as Chromium stores it).
const SEED_THEME_COLOR: i64 = -14_575_885;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub name: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub temporary: bool,
}

#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    profiles: &'a [ProfileEntry],
}

#[derive(Deserialize, Default)]
struct FileIn {
    #[serde(default)]
    profiles: Vec<ProfileEntry>,
}

/// The profile store rooted at `state_dir()/browser`.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        self.root.join(PROFILES_DIR).join(name)
    }

    pub fn log_path(&self, name: &str) -> PathBuf {
        self.root.join(PROFILES_DIR).join(format!("{name}.log"))
    }

    fn file(&self) -> PathBuf {
        self.root.join(PROFILES_FILE)
    }

    fn read(&self) -> Vec<ProfileEntry> {
        match fs::read_to_string(self.file()) {
            Ok(content) => serde_json::from_str::<FileIn>(&content)
                .map(|file| file.profiles)
                .unwrap_or_else(|err| {
                    tracing::warn!(event = "browser.profiles.load", err = %err, "profiles.json is corrupt; listing directories only");
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        }
    }

    fn write(&self, entries: &[ProfileEntry]) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let path = self.file();
        let tmp = self
            .root
            .join(format!(".{PROFILES_FILE}.tmp-{}", std::process::id()));
        let json = serde_json::to_string_pretty(&FileOut {
            version: FILE_VERSION,
            profiles: entries,
        })
        .map_err(io::Error::other)?;
        let result = fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, &path));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Every known profile: the recorded ones plus directories on disk that
    /// were never recorded (adopted as permanent).
    pub fn list(&self) -> Vec<ProfileEntry> {
        let mut entries = self.read();
        if let Ok(dir) = fs::read_dir(self.root.join(PROFILES_DIR)) {
            for entry in dir.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if !entry.path().is_dir() || !valid_profile_name(&name) {
                    continue;
                }
                if !entries.iter().any(|e| e.name == name) {
                    entries.push(ProfileEntry {
                        name: name.clone(),
                        created_at: 0,
                        temporary: name.starts_with(TEMPORARY_PREFIX),
                    });
                }
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    pub fn entry(&self, name: &str) -> Option<ProfileEntry> {
        self.list().into_iter().find(|entry| entry.name == name)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.dir(name).is_dir()
    }

    pub fn is_temporary(&self, name: &str) -> bool {
        self.entry(name).is_some_and(|entry| entry.temporary)
    }

    /// Whether the profile has been launched by herdr before.
    pub fn has_launched(&self, name: &str) -> bool {
        self.dir(name).join(LAUNCHED_MARKER).exists()
    }

    pub fn mark_launched(&self, name: &str) -> io::Result<()> {
        fs::write(self.dir(name).join(LAUNCHED_MARKER), b"")
    }

    /// `tmp-YYYYMMDD-HHMMSS` (local time).
    pub fn temporary_name(&self, now: u64) -> String {
        let stamp = time::OffsetDateTime::from_unix_timestamp(now as i64)
            .ok()
            .and_then(|t| {
                let offset =
                    time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
                let local = t.to_offset(offset);
                let format =
                    time::format_description::parse("[year][month][day]-[hour][minute][second]")
                        .ok()?;
                local.format(&format).ok()
            })
            .unwrap_or_else(|| now.to_string());
        let base = format!("{TEMPORARY_PREFIX}{stamp}");
        if !self.exists(&base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|candidate| !self.exists(candidate))
            .unwrap_or(base)
    }

    /// Create the profile directory (0700) when missing, seed its
    /// preferences and record it. Idempotent for an existing profile.
    pub fn ensure(&self, name: &str, temporary: bool, now: u64) -> Result<PathBuf, BrowserError> {
        if !valid_profile_name(name) {
            return Err(BrowserError::invalid_profile(name));
        }
        let dir = self.dir(name);
        let created = !dir.is_dir();
        if created {
            fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
                let _ = fs::set_permissions(
                    self.root.join(PROFILES_DIR),
                    fs::Permissions::from_mode(0o700),
                );
            }
            seed_preferences(&dir, name)?;
        }
        let mut entries = self.read();
        if !entries.iter().any(|entry| entry.name == name) {
            entries.push(ProfileEntry {
                name: name.to_string(),
                created_at: now,
                temporary,
            });
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            self.write(&entries)?;
        }
        Ok(dir)
    }

    /// Move the profile directory to the Trash (or `<root>/trash/` when the
    /// Trash is not available) and forget it. The caller checks it is not
    /// running.
    pub fn delete(&self, name: &str, now: u64) -> Result<PathBuf, BrowserError> {
        if !valid_profile_name(name) {
            return Err(BrowserError::invalid_profile(name));
        }
        let dir = self.dir(name);
        let mut destination = None;
        if dir.is_dir() {
            destination = Some(move_to_trash(&dir, name, now)?);
        } else if self.entry(name).is_none() {
            return Err(BrowserError::profile_not_found(name));
        }
        let _ = fs::remove_file(self.log_path(name));
        let mut entries = self.read();
        entries.retain(|entry| entry.name != name);
        self.write(&entries)?;
        Ok(destination.unwrap_or_default())
    }
}

fn move_to_trash(dir: &Path, name: &str, now: u64) -> io::Result<PathBuf> {
    let trash = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".Trash"))
        .filter(|trash| trash.is_dir())
        .unwrap_or_else(|| {
            dir.parent()
                .and_then(Path::parent)
                .map(|root| root.join("trash"))
                .unwrap_or_else(|| PathBuf::from("trash"))
        });
    fs::create_dir_all(&trash)?;
    let mut target = trash.join(format!("herdr-browser-{name}"));
    if target.exists() {
        target = trash.join(format!("herdr-browser-{name}-{now}"));
    }
    match fs::rename(dir, &target) {
        Ok(()) => Ok(target),
        Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {
            // Another volume: copy is out of scope for a profile; fall back to a
            // sibling `trash` directory on the same volume.
            let fallback_root = dir
                .parent()
                .and_then(Path::parent)
                .map(|root| root.join("trash"))
                .unwrap_or_else(|| PathBuf::from("trash"));
            fs::create_dir_all(&fallback_root)?;
            let target = fallback_root.join(format!("{name}-{now}"));
            fs::rename(dir, &target)?;
            Ok(target)
        }
        Err(err) => Err(err),
    }
}

/// Seed `Default/Preferences` so the avatar chip names the profile and the
/// theme colour tells the window apart from the user's own browser.
pub fn seed_preferences(dir: &Path, name: &str) -> io::Result<()> {
    let default = dir.join("Default");
    fs::create_dir_all(&default)?;
    let path = default.join("Preferences");
    if path.exists() {
        return Ok(());
    }
    let prefs = serde_json::json!({
        "profile": { "name": format!("herdr · {name}"), "avatar_index": SEED_AVATAR_INDEX },
        "browser": { "theme": { "user_color2": SEED_THEME_COLOR, "color_variant": 1, "is_grayscale": false } },
        "autogenerated": { "theme": { "color": SEED_THEME_COLOR } },
    });
    fs::write(
        path,
        serde_json::to_string(&prefs).map_err(io::Error::other)?,
    )
}

/// Parse `--profile` text: `new` means a fresh temporary profile.
pub fn is_new_request(name: &str) -> bool {
    name == "new"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-browser-profiles-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn ensure_creates_seeds_and_records_once() {
        let root = temp_root("ensure");
        let store = ProfileStore::new(&root);
        let dir = store.ensure("main", false, 100).unwrap();
        assert!(dir.is_dir());
        let prefs: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("Default/Preferences")).unwrap())
                .unwrap();
        assert_eq!(prefs["profile"]["name"], "herdr · main");
        assert_eq!(prefs["autogenerated"]["theme"]["color"], SEED_THEME_COLOR);
        assert!(!store.has_launched("main"));
        store.mark_launched("main").unwrap();
        assert!(store.has_launched("main"));
        // second ensure keeps the record and the preferences
        fs::write(dir.join("Default/Preferences"), "{\"edited\":1}").unwrap();
        store.ensure("main", false, 200).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("Default/Preferences")).unwrap(),
            "{\"edited\":1}"
        );
        let entries = store.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].created_at, 100);
        assert!(!entries[0].temporary);
        assert!(matches!(
            store.ensure("Bad Name", false, 1),
            Err(err) if err.code == "invalid_profile"
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn list_adopts_unrecorded_directories_and_temporary_names_are_unique() {
        let root = temp_root("list");
        let store = ProfileStore::new(&root);
        fs::create_dir_all(store.dir("stray")).unwrap();
        fs::create_dir_all(store.dir("tmp-20260930-120000")).unwrap();
        fs::create_dir_all(store.dir("Not A Profile")).unwrap();
        let entries = store.list();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["stray", "tmp-20260930-120000"]);
        assert!(store.is_temporary("tmp-20260930-120000"));
        assert!(!store.is_temporary("stray"));
        let name = store.temporary_name(1_790_762_031);
        assert!(name.starts_with("tmp-2026"), "{name}");
        store.ensure(&name, true, 1).unwrap();
        let again = store.temporary_name(1_790_762_031);
        assert_ne!(again, name);
        assert!(again.ends_with("-2"), "{again}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_moves_the_directory_away_and_forgets_the_entry() {
        let root = temp_root("delete");
        let store = ProfileStore::new(&root);
        store.ensure("scratch", true, 1).unwrap();
        fs::write(store.log_path("scratch"), "log").unwrap();
        let moved = store.delete("scratch", 5).unwrap();
        assert!(!store.exists("scratch"));
        assert!(moved.is_dir(), "{}", moved.display());
        assert!(!store.log_path("scratch").exists());
        assert!(store.entry("scratch").is_none());
        assert!(matches!(
            store.delete("scratch", 6),
            Err(err) if err.code == "profile_not_found"
        ));
        let _ = fs::remove_dir_all(&moved);
        let _ = fs::remove_dir_all(&root);
    }
}
