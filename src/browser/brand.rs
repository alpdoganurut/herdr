//! `herdr browser install-chromium` (fork, macOS): a branded copy of a built
//! Chromium.app — its own name and icon, everything else (the bundle id, the
//! compiled strings, so the "Chromium Safe Storage" keychain item keeps
//! working) untouched. Copied next to the destination, edited, ad-hoc signed,
//! verified, then swapped in atomically; an older install is replaced, a
//! running one refused.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::BrowserError;

/// The default install name; `[browser] executable = "auto"` looks for it first.
pub const DEFAULT_APP_NAME: &str = "herdr+ Browser";
/// The default icon (herdr's ram in purple on a dark rounded tile, 1024 px
/// PNG); `--icon` replaces it.
pub const DEFAULT_ICON_PNG: &[u8] =
    include_bytes!("../integration/assets/browser/branding/herdr-plus-browser-icon.png");
/// The iconset sizes an `.icns` carries: (file name, pixels).
pub const ICONSET_SIZES: [(&str, u32); 10] = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
];
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub source: PathBuf,
    pub dest_dir: PathBuf,
    pub name: String,
    pub icon: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct InstallReport {
    pub target: PathBuf,
    pub replaced: bool,
    pub strings_files: usize,
    pub icon: Option<String>,
    pub steps: Vec<String>,
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("{program}: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn install_error(message: impl Into<String>) -> BrowserError {
    BrowserError::new("install_failed", message)
}

/// A valid app name for the bundle file name: no path separators or control characters.
pub fn valid_app_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.len() <= 64
        && !name.ends_with(".app")
        && !name
            .chars()
            .any(|c| c == '/' || c == ':' || c == '\0' || c.is_control())
}

/// `plutil -replace key -string value file` (XML and binary plists, and
/// `.strings` files, which are plists too; a text OpenStep-format strings
/// file, which plutil cannot write back, is converted to binary first —
/// Chromium ships them binary anyway).
pub fn plist_set(file: &Path, key: &str, value: &str) -> Result<(), String> {
    let path = file.display().to_string();
    let replace = || run("plutil", &["-replace", key, "-string", value, &path]).map(|_| ());
    match replace() {
        Err(err) if err.contains("OpenStep") => {
            run("plutil", &["-convert", "binary1", &path])?;
            replace()
        }
        other => other,
    }
}

pub fn plist_get(file: &Path, key: &str) -> Option<String> {
    run(
        "plutil",
        &[
            "-extract",
            key,
            "raw",
            "-o",
            "-",
            &file.display().to_string(),
        ],
    )
    .ok()
    .map(|s| s.trim_end_matches('\n').to_string())
}

/// Name and display name in `Info.plist` and in every
/// `Resources/*.lproj/InfoPlist.strings` (the localized strings override the
/// plist). Returns how many strings files were edited.
pub fn brand_bundle(bundle: &Path, name: &str) -> Result<usize, String> {
    let contents = bundle.join("Contents");
    let plist = contents.join("Info.plist");
    if !plist.is_file() {
        return Err(format!("{} is not an app bundle", bundle.display()));
    }
    for key in ["CFBundleName", "CFBundleDisplayName"] {
        plist_set(&plist, key, name)?;
    }
    let mut edited = 0;
    let resources = contents.join("Resources");
    let mut lprojs: Vec<PathBuf> = std::fs::read_dir(&resources)
        .map_err(|err| format!("{}: {err}", resources.display()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "lproj"))
        .collect();
    lprojs.sort();
    for lproj in lprojs {
        let strings = lproj.join("InfoPlist.strings");
        if !strings.is_file() {
            continue;
        }
        for key in ["CFBundleName", "CFBundleDisplayName"] {
            plist_set(&strings, key, name)?;
        }
        edited += 1;
    }
    Ok(edited)
}

/// An `.icns` from a PNG (every iconset size through `sips`, then `iconutil`)
/// or a copy of a given `.icns`, written to `out`.
pub fn build_icns(icon: &Path, out: &Path, work: &Path) -> Result<String, String> {
    let ext = icon
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "icns" => {
            std::fs::copy(icon, out).map_err(|err| format!("copy icon: {err}"))?;
            Ok("icns copied".into())
        }
        "png" => {
            let iconset = work.join("herdr.iconset");
            let _ = std::fs::remove_dir_all(&iconset);
            std::fs::create_dir_all(&iconset).map_err(|err| format!("iconset dir: {err}"))?;
            for (file, px) in ICONSET_SIZES {
                let px = px.to_string();
                run(
                    "sips",
                    &[
                        "-z",
                        &px,
                        &px,
                        &icon.display().to_string(),
                        "--out",
                        &iconset.join(file).display().to_string(),
                    ],
                )?;
            }
            run(
                "iconutil",
                &[
                    "-c",
                    "icns",
                    &iconset.display().to_string(),
                    "-o",
                    &out.display().to_string(),
                ],
            )?;
            let _ = std::fs::remove_dir_all(&iconset);
            Ok(format!(
                "icns built from {} ({} sizes)",
                icon.display(),
                ICONSET_SIZES.len()
            ))
        }
        _ => Err(format!(
            "{}: the icon must be a .png or .icns",
            icon.display()
        )),
    }
}

/// Whether a process runs a binary inside `bundle` (the app is open). The
/// path is matched as given and as the kernel reports it (`/tmp` is
/// `/private/tmp` in a process's argv).
fn bundle_running(bundle: &Path) -> bool {
    let mut candidates = vec![bundle.to_path_buf()];
    if let Ok(real) = bundle.canonicalize() {
        if real != bundle {
            candidates.push(real);
        }
    }
    // A substring match over the process list: pgrep -f takes a regex, and
    // the default name has a `+` in it.
    let Ok(output) = Command::new("ps").args(["-axo", "command="]).output() else {
        return false;
    };
    let commands = String::from_utf8_lossy(&output.stdout);
    candidates.iter().any(|path| {
        let needle = format!("{}/Contents/MacOS/", path.display());
        commands.lines().any(|line| line.contains(&needle))
    })
}

pub fn install(options: &InstallOptions) -> Result<InstallReport, BrowserError> {
    let source = &options.source;
    if !source.join("Contents").join("Info.plist").is_file() {
        return Err(install_error(format!(
            "{}: not an app bundle (no Contents/Info.plist)",
            source.display()
        )));
    }
    let name = options.name.trim();
    if !valid_app_name(name) {
        return Err(install_error(format!(
            "{name:?} is not a usable app name (1–64 characters, no / or :, no .app)"
        )));
    }
    let target = options.dest_dir.join(format!("{name}.app"));
    if source.canonicalize().ok() == target.canonicalize().ok() {
        return Err(install_error(format!(
            "{} is the install itself; give the built Chromium.app",
            source.display()
        )));
    }
    if bundle_running(&target) {
        return Err(install_error(format!(
            "{} is running; quit it (herdr browser stop) and retry",
            target.display()
        )));
    }
    std::fs::create_dir_all(&options.dest_dir)
        .map_err(|err| install_error(format!("{}: {err}", options.dest_dir.display())))?;
    let stamp = std::process::id();
    let tmp = options
        .dest_dir
        .join(format!(".{name}.app.installing-{stamp}"));
    let work = options.dest_dir.join(format!(".{name}.work-{stamp}"));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&work);
    let mut report = InstallReport {
        target: target.clone(),
        ..Default::default()
    };
    let result = (|| -> Result<(), String> {
        std::fs::create_dir_all(&work).map_err(|err| format!("work dir: {err}"))?;
        run(
            "ditto",
            &[&source.display().to_string(), &tmp.display().to_string()],
        )?;
        report
            .steps
            .push(format!("copied {} → {}", source.display(), tmp.display()));
        // The icon: the given file, else the embedded default.
        let icon: PathBuf = match &options.icon {
            Some(icon) => icon.clone(),
            None => {
                let embedded = work.join("herdr-plus-browser-icon.png");
                std::fs::write(&embedded, DEFAULT_ICON_PNG)
                    .map_err(|err| format!("default icon: {err}"))?;
                embedded
            }
        };
        {
            let icon = &icon;
            let plist = tmp.join("Contents").join("Info.plist");
            let icon_file = plist_get(&plist, "CFBundleIconFile")
                .filter(|f| !f.is_empty())
                .unwrap_or_else(|| "app.icns".into());
            let icon_file = if icon_file.ends_with(".icns") {
                icon_file
            } else {
                format!("{icon_file}.icns")
            };
            let out = tmp.join("Contents").join("Resources").join(&icon_file);
            let how = build_icns(icon, &out, &work)?;
            let how = if options.icon.is_none() {
                how.replace(&icon.display().to_string(), "the embedded herdr+ icon")
            } else {
                how
            };
            report.icon = Some(format!("{icon_file}: {how}"));
            report.steps.push(format!("icon {icon_file}: {how}"));
        }
        report.strings_files = brand_bundle(&tmp, name)?;
        report.steps.push(format!(
            "named {name:?} in Info.plist and {} InfoPlist.strings",
            report.strings_files
        ));
        let _ = run(
            "xattr",
            &["-dr", "com.apple.quarantine", &tmp.display().to_string()],
        );
        run(
            "codesign",
            &["-s", "-", "-f", "--deep", &tmp.display().to_string()],
        )?;
        run(
            "codesign",
            &["--verify", "--deep", "--strict", &tmp.display().to_string()],
        )?;
        report.steps.push("ad-hoc signed and verified".into());
        // Swap in: the old install steps aside first, so the name is never half there.
        let old = options.dest_dir.join(format!(".{name}.app.old-{stamp}"));
        if target.exists() {
            std::fs::rename(&target, &old)
                .map_err(|err| format!("move old install aside: {err}"))?;
            report.replaced = true;
        }
        if let Err(err) = std::fs::rename(&tmp, &target) {
            if report.replaced {
                let _ = std::fs::rename(&old, &target);
            }
            return Err(format!("install into place: {err}"));
        }
        if report.replaced {
            let _ = std::fs::remove_dir_all(&old);
            report.steps.push("replaced the previous install".into());
        }
        match run(LSREGISTER, &["-f", &target.display().to_string()]) {
            Ok(_) => report.steps.push("LaunchServices refreshed".into()),
            Err(err) => report
                .steps
                .push(format!("LaunchServices not refreshed ({err})")),
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    match result {
        Ok(()) => Ok(report),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(install_error(err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_names_are_plain() {
        assert!(valid_app_name("herdr+ Browser"));
        assert!(!valid_app_name(""));
        assert!(!valid_app_name("a/b"));
        assert!(!valid_app_name("x.app"));
        assert!(!valid_app_name(&"n".repeat(65)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_fake_bundle_is_renamed_in_its_plist_and_every_strings_file() {
        let root = std::env::temp_dir().join(format!("herdr-brand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bundle = root.join("Fake.app");
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(contents.join("Resources/en.lproj")).unwrap();
        std::fs::create_dir_all(contents.join("Resources/tr.lproj")).unwrap();
        std::fs::create_dir_all(contents.join("Resources/empty.lproj")).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Chromium</string>
<key>CFBundleDisplayName</key><string>Chromium</string>
<key>CFBundleIdentifier</key><string>org.chromium.Chromium</string>
<key>CFBundleIconFile</key><string>app.icns</string>
</dict></plist>
"#,
        )
        .unwrap();
        // one XML strings file, one binary (as Chromium ships them)
        std::fs::write(
            contents.join("Resources/en.lproj/InfoPlist.strings"),
            "CFBundleName = \"Chromium\";\nCFBundleDisplayName = \"Chromium\";\nNSCameraUsageDescription = \"Once Chromium has access\";\n",
        )
        .unwrap();
        std::fs::write(
            contents.join("Resources/tr.lproj/InfoPlist.strings"),
            "CFBundleDisplayName = \"Chromium\";\n",
        )
        .unwrap();
        run(
            "plutil",
            &[
                "-convert",
                "binary1",
                &contents
                    .join("Resources/tr.lproj/InfoPlist.strings")
                    .display()
                    .to_string(),
            ],
        )
        .unwrap();
        let edited = brand_bundle(&bundle, "herdr Browser").unwrap();
        assert_eq!(edited, 2, "empty.lproj has no strings file");
        let plist = contents.join("Info.plist");
        assert_eq!(
            plist_get(&plist, "CFBundleName").as_deref(),
            Some("herdr Browser")
        );
        assert_eq!(
            plist_get(&plist, "CFBundleDisplayName").as_deref(),
            Some("herdr Browser")
        );
        assert_eq!(
            plist_get(&plist, "CFBundleIdentifier").as_deref(),
            Some("org.chromium.Chromium"),
            "the bundle id is never touched"
        );
        let en = contents.join("Resources/en.lproj/InfoPlist.strings");
        assert_eq!(
            plist_get(&en, "CFBundleName").as_deref(),
            Some("herdr Browser")
        );
        assert_eq!(
            plist_get(&en, "NSCameraUsageDescription").as_deref(),
            Some("Once Chromium has access"),
            "other strings stay"
        );
        let tr = contents.join("Resources/tr.lproj/InfoPlist.strings");
        assert_eq!(
            plist_get(&tr, "CFBundleDisplayName").as_deref(),
            Some("herdr Browser")
        );
        assert_eq!(
            plist_get(&tr, "CFBundleName").as_deref(),
            Some("herdr Browser")
        );
        // an install of the fake bundle: signed, swapped, icon built from a PNG
        let png = root.join("icon.png");
        run(
            "sips",
            &[
                "-s",
                "format",
                "png",
                "/System/Library/CoreServices/DefaultDesktop.heic",
                "--out",
                &png.display().to_string(),
            ],
        )
        .ok();
        let icon = png.is_file().then_some(png);
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::write(contents.join("MacOS/Fake"), "#!/bin/sh\nexit 0\n").unwrap();
        let dest = root.join("Applications");
        let options = InstallOptions {
            source: bundle.clone(),
            dest_dir: dest.clone(),
            name: "herdr Test".into(),
            icon: icon.clone(),
        };
        let first = install(&options).unwrap();
        assert!(!first.replaced);
        assert!(dest.join("herdr Test.app/Contents/Info.plist").is_file());
        assert!(
            dest.join("herdr Test.app/Contents/Resources/app.icns")
                .is_file(),
            "an icns is built from the given PNG or the embedded default"
        );
        assert!(
            DEFAULT_ICON_PNG.starts_with(b"\x89PNG"),
            "the embedded icon is a PNG"
        );
        // no --icon: the embedded default is used
        let plain = install(&InstallOptions {
            icon: None,
            name: "herdr Plain".into(),
            ..options.clone()
        })
        .unwrap();
        assert!(plain
            .icon
            .as_deref()
            .is_some_and(|i| i.contains("embedded herdr+ icon")));
        assert!(dest
            .join("herdr Plain.app/Contents/Resources/app.icns")
            .is_file());
        let second = install(&options).unwrap();
        assert!(second.replaced, "an existing install is replaced");
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            2,
            "no temp or old bundles left behind"
        );
        assert!(matches!(
            install(&InstallOptions { name: "bad/name".into(), ..options.clone() }),
            Err(err) if err.code == "install_failed"
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}
