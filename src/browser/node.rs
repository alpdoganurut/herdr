//! Node discovery for the sidecar. The server daemon usually lacks nvm's
//! PATH, so the path `herdr browser setup` recorded (`runtime.json`) matters;
//! the rest is a fallback chain.

use std::path::{Path, PathBuf};

/// Where a node binary came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeChoice {
    pub path: PathBuf,
    pub source: &'static str,
}

/// `[browser] node` → `runtime.json` → `node` on PATH → newest
/// `~/.nvm/versions/node/*/bin/node` → `/opt/homebrew/bin/node` →
/// `/usr/local/bin/node`.
pub fn discover(
    configured: Option<&str>,
    recorded: Option<&Path>,
    home: Option<&Path>,
    path_env: Option<&str>,
) -> Option<NodeChoice> {
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        if is_executable(&path) {
            return Some(NodeChoice {
                path,
                source: "config",
            });
        }
    }
    if let Some(recorded) = recorded {
        if is_executable(recorded) {
            return Some(NodeChoice {
                path: recorded.to_path_buf(),
                source: "runtime.json",
            });
        }
    }
    if let Some(path_env) = path_env {
        for dir in std::env::split_paths(path_env) {
            let candidate = dir.join("node");
            if is_executable(&candidate) {
                return Some(NodeChoice {
                    path: candidate,
                    source: "PATH",
                });
            }
        }
    }
    if let Some(home) = home {
        if let Some(path) = newest_nvm_node(home) {
            return Some(NodeChoice {
                path,
                source: "nvm",
            });
        }
    }
    for fixed in ["/opt/homebrew/bin/node", "/usr/local/bin/node"] {
        let path = PathBuf::from(fixed);
        if is_executable(&path) {
            return Some(NodeChoice {
                path,
                source: "system",
            });
        }
    }
    None
}

/// Discovery with the process environment.
pub fn discover_default(configured: Option<&str>, recorded: Option<&Path>) -> Option<NodeChoice> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path_env = std::env::var("PATH").ok();
    discover(configured, recorded, home.as_deref(), path_env.as_deref())
}

/// The `npm` beside a node binary (`<dir>/npm`), when present.
pub fn npm_beside(node: &Path) -> Option<PathBuf> {
    let npm = node.parent()?.join("npm");
    is_executable(&npm).then_some(npm)
}

fn newest_nvm_node(home: &Path) -> Option<PathBuf> {
    let versions = home.join(".nvm").join("versions").join("node");
    let mut candidates: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(versions)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let version = parse_version(&name)?;
            let node = entry.path().join("bin").join("node");
            is_executable(&node).then_some((version, node))
        })
        .collect();
    candidates.sort();
    candidates.pop().map(|(_, path)| path)
}

/// `v22.14.0` → `[22, 14, 0]`.
pub fn parse_version(name: &str) -> Option<Vec<u64>> {
    let digits = name.strip_prefix('v').unwrap_or(name);
    let parts: Vec<u64> = digits
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    (!parts.is_empty()).then_some(parts)
}

/// Whether a node binary is at least version 20 (`node --version`).
pub fn version_ok(version_output: &str) -> bool {
    parse_version(version_output.trim()).is_some_and(|parts| parts[0] >= 20)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-browser-node-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn fake_node(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(dir).unwrap();
        let node = dir.join("node");
        fs::write(&node, "#!/bin/sh\necho v22.14.0\n").unwrap();
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
        node
    }

    #[cfg(unix)]
    #[test]
    fn discovery_order_is_config_runtime_path_nvm() {
        let root = temp("order");
        let configured = fake_node(&root.join("cfg"));
        let recorded = fake_node(&root.join("rec"));
        let on_path = fake_node(&root.join("pathdir"));
        let home = root.join("home");
        fake_node(&home.join(".nvm/versions/node/v20.1.0/bin"));
        let newest = fake_node(&home.join(".nvm/versions/node/v22.14.0/bin"));
        fake_node(&home.join(".nvm/versions/node/v9.0.0/bin"));
        let path_env = root.join("pathdir").to_string_lossy().to_string();

        let pick = discover(
            Some(configured.to_str().unwrap()),
            Some(&recorded),
            Some(&home),
            Some(&path_env),
        )
        .unwrap();
        assert_eq!((pick.path, pick.source), (configured.clone(), "config"));
        let pick = discover(
            Some("/nope/node"),
            Some(&recorded),
            Some(&home),
            Some(&path_env),
        )
        .unwrap();
        assert_eq!((pick.path, pick.source), (recorded.clone(), "runtime.json"));
        let pick = discover(None, Some(Path::new("/gone")), Some(&home), Some(&path_env)).unwrap();
        assert_eq!((pick.path, pick.source), (on_path.clone(), "PATH"));
        let pick = discover(None, None, Some(&home), Some("/nowhere")).unwrap();
        assert_eq!((pick.path, pick.source), (newest, "nvm"));
        assert!(npm_beside(&configured).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn versions_parse_and_gate_at_20() {
        assert_eq!(parse_version("v22.14.0"), Some(vec![22, 14, 0]));
        assert_eq!(parse_version("18.0.0"), Some(vec![18, 0, 0]));
        assert_eq!(parse_version("latest"), None);
        assert!(version_ok("v20.0.0\n"));
        assert!(!version_ok("v18.19.1"));
        assert!(!version_ok("garbage"));
    }
}
