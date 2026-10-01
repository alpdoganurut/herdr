//! The bundled AI news runner (fork): the six files under
//! `src/integration/assets/news/`, embedded in the binary and installed
//! into `<news home>/bin/` before every run.
//!
//! Installation is idempotent: a file is written only when it is missing
//! or its bytes differ from the embedded copy (so an edited copy under
//! `bin/` is put back; the owner-editable `topic.md` and `sources.json` are
//! seeded from `bin/` into the home by the runner and never touched here).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The directory under the news home the assets are installed into.
pub const BIN_DIR: &str = "bin";
/// The entry point, relative to [`BIN_DIR`].
pub const RUNNER: &str = "news_run.py";

/// File name and content, in the order they are written.
pub const NEWS_ASSETS: &[(&str, &str)] = &[
    ("anchors.py", include_str!("assets/news/anchors.py")),
    ("viewer.py", include_str!("assets/news/viewer.py")),
    ("system.md", include_str!("assets/news/system.md")),
    ("topic.md", include_str!("assets/news/topic.md")),
    ("sources.json", include_str!("assets/news/sources.json")),
    (RUNNER, include_str!("assets/news/news_run.py")),
];

/// Where the runner lives once installed.
pub fn runner_path(home: &Path) -> PathBuf {
    home.join(BIN_DIR).join(RUNNER)
}

/// Install the assets under `<home>/bin/`, writing only what is missing or
/// stale. Returns how many files were written.
pub fn install(home: &Path) -> io::Result<usize> {
    let bin = home.join(BIN_DIR);
    fs::create_dir_all(&bin)?;
    let mut written = 0;
    for (name, content) in NEWS_ASSETS {
        let path = bin.join(name);
        let current = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        if current.as_deref() == Some(content.as_bytes()) {
            continue;
        }
        let tmp = bin.join(format!(".{name}.tmp-{}", std::process::id()));
        let result = fs::write(&tmp, content).and_then(|()| fs::rename(&tmp, &path));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        written += 1;
    }
    if written > 0 {
        tracing::info!(
            event = "news.assets.install",
            dir = %bin.display(),
            written,
            "news runner assets installed"
        );
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-news-assets-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn assets_are_the_six_files_and_the_runner_is_a_script() {
        let names: Vec<&str> = NEWS_ASSETS.iter().map(|(name, _)| *name).collect();
        assert_eq!(names.len(), 6);
        for expected in [
            "news_run.py",
            "anchors.py",
            "viewer.py",
            "system.md",
            "topic.md",
            "sources.json",
        ] {
            assert!(names.contains(&expected), "{expected} is embedded");
        }
        let runner = NEWS_ASSETS
            .iter()
            .find(|(name, _)| *name == RUNNER)
            .map(|(_, content)| *content)
            .unwrap();
        assert!(runner.starts_with("#!/usr/bin/env python3"));
        assert!(runner.contains("--trigger"));
    }

    /// The viewer and runner checks (scripts/test_news_viewer.py,
    /// scripts/test_news_run.py): marker logic, the two-leaf dealing, the
    /// validator's optional fields, the first-seen index, and a pty run of
    /// the pinned viewer. Skipped without python3 on PATH.
    #[test]
    fn python_asset_checks_pass() {
        let python = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("python3"))
                .find(|candidate| candidate.is_file())
        });
        let Some(python) = python else {
            eprintln!("python3 not on PATH; news asset checks skipped");
            return;
        };
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let output = std::process::Command::new(python)
            .args([
                "-m",
                "unittest",
                "scripts/test_news_viewer.py",
                "scripts/test_news_run.py",
            ])
            .current_dir(root)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .output()
            .expect("run python3");
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !root
                .join("src/integration/assets/news/__pycache__")
                .exists(),
            "the asset directory stays free of bytecode"
        );
    }

    #[test]
    fn install_writes_missing_files_then_only_stale_ones() {
        let home = temp_home("install");
        assert_eq!(install(&home).unwrap(), NEWS_ASSETS.len());
        assert!(runner_path(&home).is_file());
        assert_eq!(
            install(&home).unwrap(),
            0,
            "a second install writes nothing"
        );

        let viewer = home.join(BIN_DIR).join("viewer.py");
        fs::write(&viewer, "print('edited')\n").unwrap();
        assert_eq!(
            install(&home).unwrap(),
            1,
            "only the edited file is rewritten"
        );
        let restored = fs::read_to_string(&viewer).unwrap();
        assert_eq!(restored, include_str!("assets/news/viewer.py"));
        assert!(
            fs::read_dir(home.join(BIN_DIR)).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")),
            "no temp files are left behind"
        );
        let _ = fs::remove_dir_all(&home);
    }
}
