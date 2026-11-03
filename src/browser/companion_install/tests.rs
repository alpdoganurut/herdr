use std::path::{Path, PathBuf};

use super::*;
use crate::browser::setup::SetupEnv;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-companion-install-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn env_in(dir: &Path) -> SetupEnv {
    SetupEnv {
        browser_home: dir.join("browser"),
        binary: PathBuf::from("/usr/bin/true"),
        shell_file: dir.join("shell/herdr-plus.zsh"),
        zshrc: None,
        claude_json: None,
        claude_bin: None,
        claude_z_bin: None,
        codex_config: None,
        codex_bin: None,
        home: Some(dir.join("home")),
        node_override: None,
    }
}

/// An Electron `dist` directory with nothing real in it: the version file
/// and an executable where the launcher looks.
fn fake_dist(dir: &Path, version: &str) -> PathBuf {
    let dist = dir.join("electron-dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("version"), version).unwrap();
    let bundle = if cfg!(target_os = "macos") {
        let app = dist.join("Electron.app");
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>Electron</string></dict></plist>\n",
        )
        .unwrap();
        app
    } else {
        dist.clone()
    };
    let exe = bundle_executable(&bundle);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
    dist
}

fn options(dist: &Path) -> CompanionSetupOptions {
    CompanionSetupOptions {
        consent: false,
        dist_override: Some(dist.to_path_buf()),
        brand: false,
    }
}

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn fork_smoke_setup_installs_once_and_reruns_only_when_the_bundled_version_changes() {
    let dir = temp("once");
    let env = env_in(&dir);
    let config = BrowserConfig::default();
    let dist = fake_dist(&dir, ELECTRON_VERSION);
    let home = companion_home(&env.browser_home);

    // nothing installed: the check says how to install (in a terminal: the
    // download asks first, so it is no settings fix and no `!` hint)
    let before = doctor_check(&home, None);
    assert!(!before.ok);
    assert_eq!(before.fix_kind, BrowserFixKind::None);
    assert_eq!(
        before.detail,
        "not installed — herdr browser setup --companion (downloads Electron, about 115 MB)"
    );
    assert_eq!(resolve(&home), Err(InstallProblem::NotInstalled));

    let first = setup_companion(&env, &config, &options(&dist)).expect("first setup");
    assert!(first.changed, "{:?}", first.lines);
    let record = read_install(&home).expect("install.json");
    assert_eq!(record.app, COMPANION_APP_VERSION);
    assert_eq!(record.app_sha256, companion_app_sha256());
    assert_eq!(record.electron, ELECTRON_VERSION);
    assert_eq!(record.version, INSTALL_VERSION);
    let installed = resolve(&home).expect("resolves");
    assert_eq!(installed.app_dir, app_dir(&home, &companion_app_sha12()));
    assert!(installed.app_dir.join("src/main.js").is_file());
    assert!(installed
        .app_dir
        .join("assets/icon/eye-plus-512.png")
        .is_file());
    assert!(installed.executable.is_file());
    assert_eq!(
        std::fs::read_to_string(app_root(&home).join("current"))
            .unwrap()
            .trim(),
        companion_app_sha12()
    );
    assert!(first
        .lines
        .last()
        .is_some_and(|line| line.starts_with("companion: app ")
            && line.contains(&format!("electron {ELECTRON_VERSION}"))));

    // a rerun with nothing to do writes nothing
    let stamp = mtime(&home.join(INSTALL_FILE));
    std::thread::sleep(std::time::Duration::from_millis(20));
    let again = setup_companion(&env, &config, &options(&dist)).expect("rerun");
    assert!(!again.changed, "{:?}", again.lines);
    assert_eq!(again.lines, vec!["companion: up to date".to_string()]);
    assert_eq!(
        mtime(&home.join(INSTALL_FILE)),
        stamp,
        "install.json untouched"
    );
    let checked = doctor_check(&home, None);
    if cfg!(target_os = "macos") {
        // unbranded (the test skips signing): reported, not hidden
        assert!(!checked.ok);
        assert!(
            checked.detail.starts_with("bundle not signed"),
            "{checked:?}"
        );
    } else {
        assert!(checked.ok, "{checked:?}");
        assert!(checked.detail.starts_with(&format!(
            "app {COMPANION_APP_VERSION} · electron {ELECTRON_VERSION}"
        )));
    }

    // the bundled version changes (here: the record says an older one)
    let mut old = record.clone();
    old.app = "0.1.0".into();
    old.app_sha256 = "0".repeat(64);
    std::fs::write(
        home.join(INSTALL_FILE),
        serde_json::to_string_pretty(&old).unwrap(),
    )
    .unwrap();
    let outdated = doctor_check(&home, None);
    assert!(!outdated.ok);
    assert_eq!(outdated.fix_kind, BrowserFixKind::Safe);
    assert_eq!(
        outdated.detail,
        format!("app files 0.1.0 installed, {COMPANION_APP_VERSION} bundled — herdr browser setup")
    );
    let refreshed = refresh_app_files(&home, None).expect("refreshes").unwrap();
    assert!(refreshed.contains("0.1.0 →"), "{refreshed}");
    assert_eq!(
        read_install(&home).unwrap().app_sha256,
        companion_app_sha256()
    );
    assert!(
        refresh_app_files(&home, None).is_none(),
        "nothing left to do"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_app_directories_are_pruned_but_the_running_one_stays() {
    let dir = temp("prune");
    let home = dir.join("companion-app");
    let root = app_root(&home);
    for (name, age) in [
        ("aaaaaaaaaaaa", 4),
        ("bbbbbbbbbbbb", 3),
        ("cccccccccccc", 2),
    ] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("package.json"), "{}").unwrap();
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age * 60);
        let file = std::fs::File::open(root.join(name)).unwrap();
        file.set_modified(when).unwrap();
    }
    let running = root.join("aaaaaaaaaaaa");
    let files = install_app_files(&home, Some(&running)).unwrap();
    assert!(files.written);
    let mut left: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    // current + the newest older one + the running one
    let mut want = vec![
        files.sha12.clone(),
        "aaaaaaaaaaaa".to_string(),
        "cccccccccccc".to_string(),
    ];
    want.sort();
    assert_eq!(left, want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_electron_download_needs_consent_and_never_runs_from_auto_repair() {
    let dir = temp("consent");
    let env = env_in(&dir);
    let config = BrowserConfig::default();
    let home = companion_home(&env.browser_home);
    // npm's part is in place (the pinned package), the binary is not
    let package = electron_dir(&home).join("node_modules/electron");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(electron_dir(&home).join("package.json"), ELECTRON_MANIFEST).unwrap();
    std::fs::write(electron_dir(&home).join("package-lock.json"), ELECTRON_LOCK).unwrap();
    std::fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"electron\",\"version\":\"{ELECTRON_VERSION}\"}}"),
    )
    .unwrap();
    let no_consent = CompanionSetupOptions {
        consent: false,
        dist_override: None,
        brand: false,
    };
    let err = setup_companion(&env, &config, &no_consent).expect_err("asks first");
    assert_eq!(err, CompanionSetupError::NeedsConsent);
    assert_eq!(
        err.to_string(),
        "companion: Electron not downloaded — run \"herdr browser setup --companion\" in a terminal (or add --yes)"
    );
    assert!(!electron_dist(&home).exists(), "nothing downloaded");
    assert!(download_question().contains("about 115 MB"));

    // installed app files, no binary: the server's paths never download
    let mut record = InstallRecord {
        version: INSTALL_VERSION,
        app: "0.1.0".into(),
        app_sha256: "1".repeat(64),
        electron: ELECTRON_VERSION.into(),
        bundle: plain_bundle(&home).display().to_string(),
        ..Default::default()
    };
    std::fs::write(
        home.join(INSTALL_FILE),
        serde_json::to_string(&record).unwrap(),
    )
    .unwrap();
    let check = doctor_check(&home, None);
    assert_eq!(
        check.detail,
        "Electron not downloaded — herdr browser setup --companion"
    );
    assert_eq!(check.fix_kind, BrowserFixKind::None);
    // auto-repair (after a herdr update): app files only
    let _ = crate::browser::setup::auto_repair(&config, &env);
    record = read_install(&home).unwrap();
    assert_eq!(
        record.app_sha256,
        companion_app_sha256(),
        "app files refreshed"
    );
    assert!(
        !electron_dist(&home).exists(),
        "auto-repair never downloads"
    );
    // the settings fix: no download either; it names the terminal command
    let fixed = crate::browser::setup::fix("companion_app", &config, &env).unwrap();
    assert!(!fixed.ok);
    assert!(
        fixed.detail.contains("herdr browser setup --companion"),
        "{fixed:?}"
    );
    assert!(!electron_dist(&home).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dist_of_another_electron_is_refused() {
    let dir = temp("wrong-dist");
    let env = env_in(&dir);
    let dist = fake_dist(&dir, "44.0.0");
    let err = setup_companion(&env, &BrowserConfig::default(), &options(&dist)).unwrap_err();
    assert!(
        err.to_string().contains(&format!(
            "is Electron 44.0.0; this herdr needs {ELECTRON_VERSION}"
        )),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_companion_reads_as_failed_with_its_last_error() {
    let dir = temp("failed");
    let env = env_in(&dir);
    let dist = fake_dist(&dir, ELECTRON_VERSION);
    setup_companion(&env, &BrowserConfig::default(), &options(&dist)).unwrap();
    let home = companion_home(&env.browser_home);
    let live = BrowserCompanionAppInfo {
        state: "failed".into(),
        last_error: Some("exited with status 1 — boom".into()),
        ..Default::default()
    };
    let check = doctor_check(&home, Some(&live));
    assert!(!check.ok);
    assert_eq!(check.fix_kind, BrowserFixKind::None);
    assert_eq!(
        check.detail,
        "failed: 5 crashes in 5 min (last: exited with status 1 — boom) — herdr browser companion restart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Executable `body` at `dir/name`.
#[cfg(unix)]
fn script(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// An npm whose `ci` does what the real one does to `node_modules`
/// (empties it, installs the package without its binary), then exits `code`.
#[cfg(unix)]
fn npm_ci(code: u8) -> String {
    format!(
        "#!/bin/sh\n[ \"$1\" = ci ] || exit 2\necho ran >> npm-ci.log\nrm -rf node_modules\nmkdir -p node_modules/electron\nprintf '{{\"name\":\"electron\",\"version\":\"{ELECTRON_VERSION}\"}}' > node_modules/electron/package.json\nexit {code}\n"
    )
}

#[cfg(unix)]
#[test]
fn fork_smoke_a_new_lock_reruns_npm_ci_but_keeps_the_downloaded_electron() {
    let dir = temp("lock-change");
    let mut env = env_in(&dir);
    let home = companion_home(&env.browser_home);
    let electron = electron_dir(&home);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    script(&bin, "node", "#!/bin/sh\nexit 0\n");
    script(&bin, "npm", &npm_ci(0));
    env.node_override = Some(bin.join("node").display().to_string());
    // Electron downloaded under an older lock
    let dist = fake_dist(&dir, ELECTRON_VERSION);
    std::fs::create_dir_all(electron_dist(&home).parent().unwrap()).unwrap();
    std::fs::rename(&dist, electron_dist(&home)).unwrap();
    std::fs::write(
        electron.join("node_modules/electron/package.json"),
        format!("{{\"name\":\"electron\",\"version\":\"{ELECTRON_VERSION}\"}}"),
    )
    .unwrap();
    std::fs::write(electron.join("package.json"), ELECTRON_MANIFEST).unwrap();
    std::fs::write(electron.join("package-lock.json"), "{\"old\":true}\n").unwrap();
    assert_eq!(electron_installed(&home).as_deref(), Some(ELECTRON_VERSION));
    let no_consent = CompanionSetupOptions {
        consent: false,
        dist_override: None,
        brand: false,
    };

    // no download, so no consent needed; npm ci runs for the new lock
    let report = setup_companion(&env, &BrowserConfig::default(), &no_consent)
        .expect("the binary is here: nothing to ask");
    assert_eq!(
        std::fs::read_to_string(electron.join("npm-ci.log"))
            .unwrap()
            .lines()
            .count(),
        1,
        "npm ci ran"
    );
    assert_eq!(
        std::fs::read_to_string(electron.join("package-lock.json")).unwrap(),
        ELECTRON_LOCK
    );
    assert_eq!(
        electron_installed(&home).as_deref(),
        Some(ELECTRON_VERSION),
        "the binary is kept across npm ci: {:?}",
        report.lines
    );
    assert!(!kept_dist(&home).exists());
    assert!(resolve(&home).is_ok());

    // an npm ci that fails keeps it too
    script(&bin, "npm", &npm_ci(1));
    std::fs::write(electron.join("package-lock.json"), "{\"older\":true}\n").unwrap();
    let err =
        setup_companion(&env, &BrowserConfig::default(), &no_consent).expect_err("npm ci failed");
    assert!(err.to_string().contains("npm ci failed"), "{err}");
    assert_eq!(
        electron_installed(&home).as_deref(),
        Some(ELECTRON_VERSION),
        "kept across a failed npm ci"
    );

    // a setup cut short while the binary waited aside: the next one puts it back
    std::fs::rename(electron_dist(&home), kept_dist(&home)).unwrap();
    script(&bin, "npm", &npm_ci(0));
    setup_companion(&env, &BrowserConfig::default(), &no_consent).expect("recovers");
    assert_eq!(electron_installed(&home).as_deref(), Some(ELECTRON_VERSION));
    assert!(!kept_dist(&home).exists());

    // and without the binary, nothing runs before the consent (npm included)
    std::fs::remove_dir_all(electron_dist(&home)).unwrap();
    std::fs::write(electron.join("package-lock.json"), "{\"oldest\":true}\n").unwrap();
    assert_eq!(
        setup_companion(&env, &BrowserConfig::default(), &no_consent),
        Err(CompanionSetupError::NeedsConsent)
    );
    assert_eq!(
        std::fs::read_to_string(electron.join("npm-ci.log"))
            .unwrap()
            .lines()
            .count(),
        2,
        "npm ci did not run again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_settings_fix_keeps_the_app_files_a_companion_still_runs_from() {
    let dir = temp("fix-keeps");
    let env = env_in(&dir);
    let dist = fake_dist(&dir, ELECTRON_VERSION);
    setup_companion(&env, &BrowserConfig::default(), &options(&dist)).unwrap();
    let home = companion_home(&env.browser_home);
    let root = app_root(&home);
    for (name, age) in [
        ("aaaaaaaaaaaa", 4),
        ("bbbbbbbbbbbb", 3),
        ("cccccccccccc", 2),
    ] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("package.json"), "{}").unwrap();
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age * 60);
        std::fs::File::open(root.join(name))
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
    // the oldest is a live companion's (this test process stands in for it)
    crate::browser::companion_app::write_run_record(
        &home,
        &crate::browser::companion_app::CompanionRunRecord {
            pid: std::process::id(),
            launch_token: "t".into(),
            app_dir: root.join("aaaaaaaaaaaa").display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    // the installed app files are an older herdr's
    let mut record = read_install(&home).unwrap();
    record.app = "0.1.0".into();
    record.app_sha256 = format!("{}{}", "c".repeat(12), "0".repeat(52));
    std::fs::write(
        home.join(INSTALL_FILE),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();
    let fixed = crate::browser::setup::fix("companion_app", &BrowserConfig::default(), &env)
        .expect("a fix for companion_app");
    assert!(fixed.ok, "{fixed:?}");
    assert!(
        root.join("aaaaaaaaaaaa").is_dir(),
        "a running companion's directory stays"
    );
    assert!(!root.join("bbbbbbbbbbbb").exists(), "older ones go");
    let _ = std::fs::remove_dir_all(&dir);
}
