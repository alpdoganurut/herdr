//! What a linked browser may read of this machine, and what not.

use super::*;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `<tmp>/site/{index.html, a b/style.css}` and a secret beside `site`.
fn site(name: &str) -> (Dir, PathBuf) {
    let base = std::env::temp_dir().join(format!("herdr-link-files-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let site = base.join("site");
    std::fs::create_dir_all(site.join("a b")).unwrap();
    std::fs::write(site.join("index.html"), "<h1>hi</h1>").unwrap();
    std::fs::write(site.join("a b/style.css"), "h1{}").unwrap();
    std::fs::write(base.join("secret.txt"), "secret").unwrap();
    let site = std::fs::canonicalize(site).unwrap();
    (Dir(base), site)
}

fn pull(grants: &FileGrants, token: &str, path: &str, epoch: u64) -> FileReply {
    grants.serve(
        &FileRequest {
            id: 7,
            token: token.into(),
            path: path.into(),
            offset: 0,
        },
        epoch,
    )
}

#[test]
fn a_shown_file_and_its_neighbours_are_served_under_one_token() {
    let (_dir, site) = site("serve");
    let grants = FileGrants::default();
    let shown = grants.shown(&site.join("index.html"), 3).unwrap();
    assert!(is_file_token(&shown.token), "{}", shown.token);
    assert_eq!(shown.rel, "index.html");
    assert_eq!(
        shown.url(),
        format!("herdr-file://{}/index.html", shown.token)
    );
    let page = pull(&grants, &shown.token, "index.html", 3);
    assert_eq!(
        (
            page.id,
            page.status,
            page.mime.as_str(),
            page.size,
            page.eof
        ),
        (7, 200, "text/html", 11, true)
    );
    assert_eq!(page.data, b"<h1>hi</h1>");
    let css = pull(&grants, &shown.token, "a%20b/style.css", 3);
    assert_eq!((css.status, css.mime.as_str()), (200, "text/css"));
    // one directory, one token per epoch
    assert_eq!(grants.token_for(&site, 3), shown.token);
    assert_ne!(grants.token_for(&site, 4), shown.token);
}

#[test]
fn nothing_outside_the_directory_is_served() {
    let (dir, site) = site("outside");
    let grants = FileGrants::default();
    let token = grants.token_for(&site, 1);
    for path in [
        "../secret.txt",
        "%2e%2e/secret.txt",
        "%2E%2E/secret.txt",
        "a%20b/../../secret.txt",
        "./index.html",
        "%2fetc/passwd",
        "a%20b%2f..%2f..%2fsecret.txt",
        "a%5c..%5csecret.txt",
        "a\\..\\secret.txt",
        "index.html%00.png",
        "index.html?x=1",
        "index.html#top",
        "a%20b//style.css",
        "",
        "%zz",
        "%ff%fe",
        "a\u{7f}b",
    ] {
        assert_eq!(pull(&grants, &token, path, 1).status, 403, "{path:?}");
    }
    // a link that leads out of it, and one that stays inside
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.0.join("secret.txt"), site.join("out.txt")).unwrap();
        std::os::unix::fs::symlink(site.join("index.html"), site.join("in.html")).unwrap();
        assert_eq!(pull(&grants, &token, "out.txt", 1).status, 403);
        assert_eq!(pull(&grants, &token, "in.html", 1).status, 200);
    }
    // missing, a directory
    assert_eq!(pull(&grants, &token, "nope.html", 1).status, 404);
    assert_eq!(pull(&grants, &token, "a%20b", 1).status, 404);
}

#[test]
fn a_token_serves_only_in_its_own_epoch() {
    let (_dir, site) = site("epoch");
    let grants = FileGrants::default();
    let old = grants.token_for(&site, 1);
    let new = grants.token_for(&site, 2);
    assert_eq!(pull(&grants, &old, "index.html", 2).status, 403);
    assert_eq!(pull(&grants, &new, "index.html", 2).status, 200);
    assert_eq!(pull(&grants, &new, "index.html", 1).status, 403);
    assert_eq!(pull(&grants, &"a".repeat(24), "index.html", 2).status, 403);
    // its URLs still say which file they were
    let file = format!("file://{}/a%20b/style.css?v=2#x", site.display());
    for token in [&old, &new] {
        assert_eq!(
            grants
                .to_file_url(&format!("herdr-file://{token}/a%20b/style.css?v=2#x"))
                .as_deref(),
            Some(file.as_str())
        );
    }
    for other in [
        "https://example.test/",
        "herdr-file://notatoken/index.html",
        &format!("herdr-file://{new}/../secret.txt"),
        &format!("herdr-file://{}/index.html", "b".repeat(24)),
    ] {
        assert_eq!(grants.to_file_url(other), None, "{other}");
    }
}

#[test]
fn a_large_file_comes_in_chunks_and_one_over_the_cap_not_at_all() {
    let (_dir, site) = site("size");
    let big = vec![b'x'; CHUNK_BYTES * 2 + 5];
    std::fs::write(site.join("big.js"), &big).unwrap();
    let grants = FileGrants::default();
    let token = grants.token_for(&site, 1);
    let mut got = Vec::new();
    let mut offset = 0;
    loop {
        let reply = grants.serve(
            &FileRequest {
                id: 1,
                token: token.clone(),
                path: "big.js".into(),
                offset,
            },
            1,
        );
        assert_eq!((reply.status, reply.size), (200, big.len() as u64));
        assert!(reply.data.len() <= CHUNK_BYTES);
        offset += reply.data.len() as u64;
        got.extend(reply.data);
        if reply.eof {
            break;
        }
    }
    assert_eq!(got, big);
    let file = std::fs::File::create(site.join("huge.bin")).unwrap();
    file.set_len(FILE_MAX_BYTES + 1).unwrap();
    assert_eq!(pull(&grants, &token, "huge.bin", 1).status, 403);
    file.set_len(FILE_MAX_BYTES).unwrap();
    assert_eq!(pull(&grants, &token, "huge.bin", 1).status, 200);
}
