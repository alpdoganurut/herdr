//! The files a linked browser shows (fork, remote browser): `show` on a
//! linked server grants the shown file's directory to the Mac under a
//! token, and the Mac's companion pulls the page and everything it loads
//! from that directory with `file` frames (`herdr-file://<token>/<rel>`).
//!
//! The Mac never sees a path of this machine. A token serves only during
//! the link epoch it was minted in (the companion asks again with the
//! entry's current token); an older one still maps its URLs back to the
//! file they were, so pages, records and the ledger keep this machine's
//! paths.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use super::link_proto::{
    is_file_token, FileReply, FileRequest, CHUNK_BYTES, FILE_MAX_BYTES, FILE_SCHEME,
};
use super::session::random_hex;

/// The grants a server process keeps, oldest epochs first to go.
const GRANTS_MAX: usize = 4096;

#[derive(Debug, Clone)]
struct Grant {
    epoch: u64,
    /// Canonical.
    root: PathBuf,
}

#[derive(Default)]
struct Grants {
    by_token: HashMap<String, Grant>,
    /// The token of a root in an epoch.
    by_root: HashMap<(u64, PathBuf), String>,
}

/// The tokens of this server process.
#[derive(Default)]
pub struct FileGrants {
    grants: Mutex<Grants>,
}

/// What a page of a linked browser is on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub token: String,
    /// Percent-encoded, without a leading `/`.
    pub rel: String,
}

impl FileGrants {
    fn grants(&self) -> std::sync::MutexGuard<'_, Grants> {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The token under which `root` (canonical) is served in `epoch`: the
    /// same one for every file of the directory.
    pub fn token_for(&self, root: &Path, epoch: u64) -> String {
        let mut grants = self.grants();
        if let Some(token) = grants.by_root.get(&(epoch, root.to_path_buf())) {
            return token.clone();
        }
        if grants.by_token.len() >= GRANTS_MAX {
            let oldest = grants.by_token.values().map(|grant| grant.epoch).min();
            grants
                .by_token
                .retain(|_, grant| Some(grant.epoch) != oldest);
            grants
                .by_root
                .retain(|(epoch, _), _| Some(*epoch) != oldest);
        }
        let token = loop {
            let token = format!("h{}", random_hex(23));
            if !grants.by_token.contains_key(&token) {
                break token;
            }
        };
        grants.by_token.insert(
            token.clone(),
            Grant {
                epoch,
                root: root.to_path_buf(),
            },
        );
        grants
            .by_root
            .insert((epoch, root.to_path_buf()), token.clone());
        token
    }

    /// `file` in `epoch`: the chunk at its offset, or 403 (a token of
    /// another epoch or none, a path that is none or leaves its directory, a
    /// file over [`FILE_MAX_BYTES`]) or 404 (nothing there, or no file).
    pub fn serve(&self, request: &FileRequest, epoch: u64) -> FileReply {
        let root = {
            let grants = self.grants();
            match grants.by_token.get(&request.token) {
                Some(grant) if grant.epoch == epoch => grant.root.clone(),
                _ => return status(request.id, 403),
            }
        };
        let Some(rel) = decode_rel(&request.path) else {
            return status(request.id, 403);
        };
        let Ok(file) = std::fs::canonicalize(root.join(rel)) else {
            return status(request.id, 404);
        };
        // a link inside the directory may not lead out of it
        if !file.starts_with(&root) {
            return status(request.id, 403);
        }
        match read_chunk(&file, request.offset) {
            Ok(Chunk::Data { size, data, eof }) => FileReply {
                id: request.id,
                status: 200,
                mime: mime(&file).into(),
                size,
                data,
                eof,
            },
            Ok(Chunk::TooLarge) => status(request.id, 403),
            Ok(Chunk::NoFile) | Err(_) => status(request.id, 404),
        }
    }

    /// `herdr-file://<token>/<rel>` as the `file://` URL it is here, for a
    /// token of any epoch of this process; `None` for every other URL.
    pub fn to_file_url(&self, url: &str) -> Option<String> {
        let (token, rest) = split_file_url(url)?;
        let root = self.grants().by_token.get(token)?.root.clone();
        let (path, suffix) = match rest.find(['?', '#']) {
            Some(at) => rest.split_at(at),
            None => (rest, ""),
        };
        let rel = decode_rel(path)?;
        Some(format!(
            "file://{}{suffix}",
            encode_path(&root.join(rel).display().to_string())
        ))
    }

    /// A file shown here, as its page on the Mac in `epoch`.
    pub fn shown(&self, file: &Path, epoch: u64) -> Option<Shown> {
        let file = std::fs::canonicalize(file).ok()?;
        let root = file.parent()?;
        let name = file.file_name()?.to_str()?;
        Some(Shown {
            token: self.token_for(root, epoch),
            rel: encode_segment(name),
        })
    }
}

impl Shown {
    /// `herdr-file://<token>/<rel>`.
    #[cfg(test)]
    pub fn url(&self) -> String {
        format!("{FILE_SCHEME}://{}/{}", self.token, self.rel)
    }
}

fn status(id: u64, status: u16) -> FileReply {
    FileReply {
        id,
        status,
        mime: String::new(),
        size: 0,
        data: Vec::new(),
        eof: true,
    }
}

enum Chunk {
    Data { size: u64, data: Vec<u8>, eof: bool },
    TooLarge,
    NoFile,
}

fn read_chunk(file: &Path, offset: u64) -> std::io::Result<Chunk> {
    let meta = std::fs::metadata(file)?;
    if !meta.is_file() {
        return Ok(Chunk::NoFile);
    }
    let size = meta.len();
    if size > FILE_MAX_BYTES {
        return Ok(Chunk::TooLarge);
    }
    let mut data = Vec::new();
    if offset < size {
        let mut handle = std::fs::File::open(file)?;
        handle.seek(SeekFrom::Start(offset))?;
        handle.take(CHUNK_BYTES as u64).read_to_end(&mut data)?;
    }
    let eof = offset + data.len() as u64 >= size;
    Ok(Chunk::Data { size, data, eof })
}

/// `herdr-file://<token>/<rest>`: the token and the rest.
fn split_file_url(url: &str) -> Option<(&str, &str)> {
    let rest = url
        .get(..FILE_SCHEME.len() + 3)
        .filter(|scheme| scheme.eq_ignore_ascii_case(&format!("{FILE_SCHEME}://")))
        .map(|_| &url[FILE_SCHEME.len() + 3..])?;
    let (token, rest) = rest.split_once('/')?;
    // the companion's URLs fold hosts to lower case
    is_file_token(token).then_some((token, rest))
}

/// A URL path (percent-encoded, no leading `/`) as a relative path; `None`
/// for one that is none: empty segments, `.` or `..` (also encoded), an
/// encoded `/`, `\` or NUL, a raw `\`, `?`, `#` or control character.
pub fn decode_rel(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.contains(['\\', '?', '#']) || path.contains(char::is_control) {
        return None;
    }
    let mut rel = PathBuf::new();
    for segment in path.split('/') {
        let decoded = percent_decode(segment)?;
        if decoded.is_empty()
            || decoded == "."
            || decoded == ".."
            || decoded.contains(['/', '\\'])
            || decoded.contains(char::is_control)
        {
            return None;
        }
        rel.push(decoded);
    }
    Some(rel)
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = segment.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// One path segment for a URL: unreserved characters stay, the rest is
/// `%XX`.
pub fn encode_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// An absolute path for a `file://` URL (each segment encoded).
fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// The type a page is served as.
fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" | "cjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests;
