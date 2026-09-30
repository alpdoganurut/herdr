//! Screenshot files: naming under `shots/<profile>/`, the downscaled inline
//! copy's path, and retention of herdr-named files.

use std::fs;
use std::path::{Path, PathBuf};

use super::BrowserError;

pub const SHOTS_DIR: &str = "shots";
const INLINE_SUFFIX: &str = ".inline";

/// `(full-size path, inline copy path)`. With `out` the full-size file goes
/// there (parent must exist) and the inline copy still lands in `shots/`.
pub fn paths(
    shots_dir: &Path,
    short: &str,
    format: &str,
    out: Option<&str>,
    now: u64,
) -> Result<(String, String), BrowserError> {
    fs::create_dir_all(shots_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(shots_dir, fs::Permissions::from_mode(0o700));
    }
    let ext = if format == "png" { "png" } else { "jpg" };
    let stamp = stamp(now);
    let mut base = shots_dir.join(format!("{stamp}-{short}.{ext}"));
    let mut n = 1;
    while base.exists() {
        n += 1;
        base = shots_dir.join(format!("{stamp}-{short}-{n}.{ext}"));
    }
    let inline = base.with_file_name(format!(
        "{}{INLINE_SUFFIX}.{ext}",
        base.file_stem().and_then(|s| s.to_str()).unwrap_or("shot")
    ));
    let full = match out {
        Some(out) => {
            let out = PathBuf::from(out);
            let out = if out.is_absolute() {
                out
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(&out))
                    .unwrap_or(out)
            };
            if let Some(parent) = out.parent() {
                if !parent.is_dir() {
                    return Err(BrowserError::new(
                        "invalid_request",
                        format!("--out: directory {} does not exist", parent.display()),
                    ));
                }
            }
            out
        }
        None => base,
    };
    Ok((full.display().to_string(), inline.display().to_string()))
}

/// `YYYYMMDD-HHMMSS` local.
pub fn stamp(now: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(now as i64)
        .ok()
        .and_then(|t| {
            let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
            let format =
                time::format_description::parse("[year][month][day]-[hour][minute][second]")
                    .ok()?;
            t.to_offset(offset).format(&format).ok()
        })
        .unwrap_or_else(|| now.to_string())
}

/// Keep the newest `keep` herdr-named files (by name, which sorts by time);
/// files not matching the `<stamp>-tN` pattern are left alone.
pub fn prune(shots_dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(shots_dir) else {
        return;
    };
    let mut ours: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_herdr_named)
        })
        .collect();
    if ours.len() <= keep {
        return;
    }
    ours.sort();
    let excess = ours.len() - keep;
    for path in ours.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}

fn is_herdr_named(name: &str) -> bool {
    // 20260930-120102-t3.jpg / 20260930-120102-t3-2.png / …-t3.inline.jpg
    let mut parts = name.splitn(3, '-');
    let (Some(date), Some(time), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
        && time.len() == 6
        && time.bytes().all(|b| b.is_ascii_digit())
        && rest.starts_with('t')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-browser-shots-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn names_are_stamped_unique_and_inline_copies_sit_beside() {
        let dir = temp("names");
        let (full, inline) = paths(&dir, "t3", "jpeg", None, 1_790_762_031).unwrap();
        assert!(full.ends_with("-t3.jpg"), "{full}");
        assert!(inline.ends_with("-t3.inline.jpg"), "{inline}");
        fs::write(&full, "x").unwrap();
        let (again, _) = paths(&dir, "t3", "jpeg", None, 1_790_762_031).unwrap();
        assert!(again.ends_with("-t3-2.jpg"), "{again}");
        let (png, inline_png) = paths(&dir, "t1", "png", None, 1).unwrap();
        assert!(png.ends_with(".png") && inline_png.ends_with(".inline.png"));
        let out = dir.join("custom.png");
        let (full, _) = paths(&dir, "t1", "png", Some(out.to_str().unwrap()), 1).unwrap();
        assert_eq!(full, out.display().to_string());
        assert!(
            matches!(paths(&dir, "t1", "png", Some("/nope/dir/x.png"), 1), Err(err) if err.code == "invalid_request")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_the_newest_and_ignores_foreign_files() {
        let dir = temp("prune");
        fs::create_dir_all(&dir).unwrap();
        for i in 0..5 {
            fs::write(dir.join(format!("20260930-12000{i}-t1.jpg")), "x").unwrap();
        }
        fs::write(dir.join("mine.jpg"), "x").unwrap();
        prune(&dir, 2);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "20260930-120003-t1.jpg",
                "20260930-120004-t1.jpg",
                "mine.jpg"
            ]
        );
        assert!(is_herdr_named("20260930-120102-t3.inline.jpg"));
        assert!(!is_herdr_named("shot.jpg"));
        let _ = fs::remove_dir_all(&dir);
    }
}
