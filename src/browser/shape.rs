//! Output shaping: the one-line header every result starts with, paging with
//! a `next:` footer, the page card, `find` windows, tables. All of it lives
//! here so the CLI and the MCP server print byte-identical text and there is
//! one place to tune token cost.

use serde_json::{json, Value};

use super::host::PageInfo;
use super::state::{display_url, BrowserTabRecord, TabKey};
use super::BrowserError;
use crate::api::schema::BrowserRunResult;

/// Longest title in a header.
const HEADER_TITLE_CHARS: usize = 60;
/// Longest URL in a header.
const HEADER_URL_CHARS: usize = 80;
/// Characters of main-content markdown the `open` card shows.
const CARD_PREVIEW_CHARS: usize = 1500;

/// Page-controlled text (titles, URLs, body) without C0/C1 control
/// characters, so nothing a page says can reach the terminal as an escape
/// sequence (OSC 52 clipboard writes, cursor games). `\n` and `\t` stay.
pub fn sanitize(text: &str) -> String {
    if !text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return text.to_string();
    }
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

fn clip(text: &str, max: usize) -> String {
    let text = sanitize(text);
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn one_line(text: &str) -> String {
    sanitize(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `[t3 · 200 · github.com/foo/bar · "title" · dialog open]`
pub fn header(record: &BrowserTabRecord, page: Option<&PageInfo>) -> String {
    let url = page
        .filter(|p| !p.url.is_empty())
        .map(|p| p.url.as_str())
        .unwrap_or(&record.url);
    let title = page
        .filter(|p| !p.title.is_empty())
        .map(|p| p.title.as_str())
        .unwrap_or(&record.title);
    let mut parts = vec![record.id()];
    if let Some(status) = page.and_then(|p| p.status) {
        parts.push(status.to_string());
    }
    parts.push(clip(&display_url(url), HEADER_URL_CHARS));
    if !title.is_empty() {
        parts.push(format!(
            "\"{}\"",
            clip(&one_line(title), HEADER_TITLE_CHARS)
        ));
    }
    if page.is_some_and(|p| p.dialog_open) || record.dialog_open {
        parts.push("dialog open".into());
    }
    format!("[{}]", parts.join(" · "))
}

/// Accept `example.com/x` as `https://example.com/x`; refuse non-web schemes.
pub fn normalize_url(url: &str) -> Result<String, BrowserError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(BrowserError::new("invalid_request", "a URL is required"));
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("file://")
        || lower == "about:blank"
        || lower.starts_with("data:")
    {
        return Ok(url.to_string());
    }
    if lower.starts_with("about:") {
        return Err(BrowserError::new(
            "invalid_request",
            "only about:blank is allowed among about: URLs",
        ));
    }
    if let Some((scheme, _)) = url.split_once("://") {
        return Err(BrowserError::new(
            "invalid_request",
            format!("unsupported URL scheme {scheme:?}; use http(s), file or about"),
        ));
    }
    if lower.starts_with("javascript:") || lower.starts_with("chrome:") {
        return Err(BrowserError::new(
            "invalid_request",
            "that URL scheme is not allowed",
        ));
    }
    Ok(format!("https://{url}"))
}

/// `markdown` (default), `text`, `snapshot`, `html`.
pub fn read_format(format: Option<&str>) -> Result<&'static str, BrowserError> {
    Ok(match format.unwrap_or("markdown") {
        "markdown" | "md" => "markdown",
        "text" | "txt" => "text",
        "snapshot" | "aria" => "snapshot",
        "html" => "html",
        other => {
            return Err(BrowserError::new(
                "invalid_request",
                format!("read format {other:?}: expected markdown, text, snapshot or html"),
            ))
        }
    })
}

/// A page of a long text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paged {
    pub text: String,
    pub offset: u64,
    pub end: u64,
    pub total: u64,
    pub truncated: bool,
}

impl Paged {
    pub fn footer(&self, next_hint: &str) -> String {
        if self.truncated {
            format!(
                "[chars {}–{} of {} · next: {next_hint} --offset {}]",
                self.offset, self.end, self.total, self.end
            )
        } else if self.offset > 0 {
            format!(
                "[chars {}–{} of {} · end]",
                self.offset, self.end, self.total
            )
        } else {
            format!("[{} chars]", self.total)
        }
    }

    /// `20k/48k` for the activity log.
    pub fn detail(&self) -> String {
        fn k(n: u64) -> String {
            if n >= 1000 {
                format!("{}k", n / 1000)
            } else {
                n.to_string()
            }
        }
        if self.truncated || self.offset > 0 {
            format!("{}–{}/{}", k(self.offset), k(self.end), k(self.total))
        } else {
            k(self.total)
        }
    }
}

/// Slice `[offset, offset+max)` in characters; `max = None` returns the rest.
pub fn page_text(content: &str, offset: u64, max: Option<u64>) -> Paged {
    let total = content.chars().count() as u64;
    let offset = offset.min(total);
    let end = match max {
        Some(max) => (offset + max).min(total),
        None => total,
    };
    let text: String = content
        .chars()
        .skip(offset as usize)
        .take((end - offset) as usize)
        .collect();
    Paged {
        text,
        offset,
        end,
        total,
        truncated: end < total,
    }
}

fn base(record: &BrowserTabRecord, page: Option<&PageInfo>) -> BrowserRunResult {
    BrowserRunResult {
        header: header(record, page),
        tab: Some(record.id()),
        ..Default::default()
    }
}

pub fn simple_result(
    record: &BrowserTabRecord,
    page: Option<&PageInfo>,
    text: &str,
    data: Value,
) -> BrowserRunResult {
    let mut result = base(record, page);
    result.text = text.to_string();
    result.data = data;
    result
}

/// The `open` card: counts plus the first part of the main content.
pub fn open_result(record: &BrowserTabRecord, page: &PageInfo, host: &Value) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let card = &host["card"];
    let mut lines = Vec::new();
    if card.is_object() {
        lines.push(format!(
            "headings {} · links {} · forms {} · text ≈ {} · main ≈ {}",
            card["headings"].as_u64().unwrap_or(0),
            card["links"].as_u64().unwrap_or(0),
            card["forms"].as_u64().unwrap_or(0),
            approx(card["text_chars"].as_u64().unwrap_or(0)),
            approx(card["main_chars"].as_u64().unwrap_or(0)),
        ));
        if card["login_wall"].as_bool().unwrap_or(false) {
            lines.push("login wall? a password field is visible or the URL looks like a sign-in page; ask the user to log in (browser focus) and retry".into());
        }
        if let Some(preview) = card["preview"].as_str() {
            if !preview.trim().is_empty() {
                lines.push(String::new());
                lines.push(clip(preview.trim_end(), CARD_PREVIEW_CHARS));
                if card["main_chars"].as_u64().unwrap_or(0) > CARD_PREVIEW_CHARS as u64 {
                    lines.push(String::new());
                    lines.push("[more: browser read]".into());
                }
            }
        }
    }
    result.text = lines.join("\n");
    result.data = json!({
        "tab": record.id(),
        "url": page.url,
        "title": page.title,
        "status": page.status,
        "card": card,
    });
    result
}

fn approx(n: u64) -> String {
    if n >= 1000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

pub fn nav_result(record: &BrowserTabRecord, page: &PageInfo, host: &Value) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let mut lines = Vec::new();
    if let Some(warning) = host["warning"].as_str() {
        lines.push(format!("warning: {warning}"));
    }
    if host["navigated_during"].as_bool().unwrap_or(false) {
        lines.push("note: the page navigated while the command ran (user or script); the result reflects the new page".into());
    }
    result.text = lines.join("\n");
    result.data =
        json!({ "tab": record.id(), "url": page.url, "title": page.title, "status": page.status });
    result
}

pub fn read_result(
    record: &BrowserTabRecord,
    page: &PageInfo,
    format: &str,
    paged: &Paged,
    ref_: Option<&str>,
    selector: Option<&str>,
) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let mut hint = format!("browser read {}", record.short);
    if format != "markdown" {
        hint.push_str(&format!(" --format {format}"));
    }
    if let Some(r) = ref_ {
        hint.push_str(&format!(" --ref {r}"));
    }
    if let Some(s) = selector {
        hint.push_str(&format!(" --selector {s:?}"));
    }
    result.text = format!("{}\n{}", sanitize(&paged.text), paged.footer(&hint));
    result.data = json!({
        "tab": record.id(),
        "format": format,
        "content": paged.text,
        "offset": paged.offset,
        "end": paged.end,
        "total_chars": paged.total,
        "truncated": paged.truncated,
        "next_offset": if paged.truncated { Some(paged.end) } else { None },
    });
    result
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FindMatch {
    pub offset: u64,
    pub context: String,
}

/// Case-insensitive substring or `/regex/` search over the markdown.
pub fn find_matches(
    content: &str,
    query: &str,
    max: usize,
    context: usize,
) -> Result<Vec<FindMatch>, BrowserError> {
    let chars: Vec<char> = content.chars().collect();
    let regex = if query.len() >= 2 && query.starts_with('/') && query.ends_with('/') {
        Some(
            regex::RegexBuilder::new(&query[1..query.len() - 1])
                .case_insensitive(true)
                .build()
                .map_err(|err| BrowserError::new("invalid_request", format!("bad regex: {err}")))?,
        )
    } else {
        None
    };
    // Byte offsets from the regex / substring search become char offsets.
    let mut byte_to_char = Vec::with_capacity(content.len() + 1);
    for (i, (byte_index, _)) in content.char_indices().enumerate() {
        while byte_to_char.len() < byte_index {
            byte_to_char.push(i);
        }
        byte_to_char.push(i);
    }
    while byte_to_char.len() <= content.len() {
        byte_to_char.push(chars.len());
    }
    let mut hits: Vec<(usize, usize)> = Vec::new();
    match &regex {
        Some(regex) => {
            for m in regex.find_iter(content) {
                hits.push((byte_to_char[m.start()], byte_to_char[m.end()]));
                if hits.len() >= max {
                    break;
                }
            }
        }
        None => {
            let lower = content.to_lowercase();
            let needle = query.to_lowercase();
            if needle.is_empty() {
                return Err(BrowserError::new("invalid_request", "find needs a query"));
            }
            // Work on the lowercase string; map back through char counts.
            let lower_chars: Vec<char> = lower.chars().collect();
            let needle_chars: Vec<char> = needle.chars().collect();
            let mut i = 0;
            while i + needle_chars.len() <= lower_chars.len() {
                if lower_chars[i..i + needle_chars.len()] == needle_chars[..] {
                    hits.push((
                        i.min(chars.len()),
                        (i + needle_chars.len()).min(chars.len()),
                    ));
                    if hits.len() >= max {
                        break;
                    }
                    i += needle_chars.len();
                } else {
                    i += 1;
                }
            }
        }
    }
    let half = context / 2;
    Ok(hits
        .into_iter()
        .map(|(start, end)| {
            let from = start.saturating_sub(half);
            let to = (end + half).min(chars.len());
            let window: String = chars[from..to].iter().collect();
            let mut text = one_line(&window);
            if from > 0 {
                text.insert(0, '…');
            }
            if to < chars.len() {
                text.push('…');
            }
            FindMatch {
                offset: start as u64,
                context: text,
            }
        })
        .collect())
}

pub fn find_result(
    record: &BrowserTabRecord,
    page: &PageInfo,
    query: &str,
    matches: &[FindMatch],
    total_chars: usize,
) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let mut lines: Vec<String> = matches
        .iter()
        .map(|m| format!("@{} {}", m.offset, m.context))
        .collect();
    if lines.is_empty() {
        lines.push(format!(
            "no match for {query:?} in {total_chars} chars of markdown"
        ));
    } else {
        lines.push(format!(
            "[{} matches · read around one: browser read {} --offset N]",
            matches.len(),
            record.short
        ));
    }
    result.text = lines.join("\n");
    result.data = json!({ "tab": record.id(), "query": query, "matches": matches, "total_chars": total_chars });
    result
}

pub fn links_result(record: &BrowserTabRecord, page: &PageInfo, host: &Value) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let links = host["links"].as_array().cloned().unwrap_or_default();
    let mut lines: Vec<String> = links
        .iter()
        .map(|link| {
            format!(
                "{} → {}",
                clip(&one_line(link["text"].as_str().unwrap_or("")), 80),
                sanitize(link["href"].as_str().unwrap_or(""))
            )
        })
        .collect();
    let total = host["total"].as_u64().unwrap_or(links.len() as u64);
    lines.push(format!("[{} of {} links]", links.len(), total));
    result.text = lines.join("\n");
    result.data = json!({ "tab": record.id(), "links": links, "total": total });
    result
}

pub fn screenshot_result(
    record: &BrowserTabRecord,
    page: &PageInfo,
    host: &Value,
    format: &str,
) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let path = host["path"].as_str().unwrap_or("").to_string();
    let inline = host["inline_path"].as_str().map(str::to_string);
    result.text = format!(
        "screenshot {}x{} → {}",
        host["width"].as_u64().unwrap_or(0),
        host["height"].as_u64().unwrap_or(0),
        path
    );
    result.image_mime = Some(
        if format == "png" {
            "image/png"
        } else {
            "image/jpeg"
        }
        .into(),
    );
    result.image_path = Some(path.clone());
    result.image_inline_path = inline.clone();
    result.data = json!({ "tab": record.id(), "path": path, "inline_path": inline, "width": host["width"], "height": host["height"] });
    result
}

pub fn console_result(
    record: &BrowserTabRecord,
    page: &PageInfo,
    host: &Value,
) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let entries = host["entries"].as_array().cloned().unwrap_or_default();
    let mut lines: Vec<String> = entries
        .iter()
        .map(|e| {
            let loc = match (e["url"].as_str(), e["line"].as_u64()) {
                (Some(url), Some(line)) if !url.is_empty() => {
                    format!(" ({}:{line})", clip(&display_url(url), 50))
                }
                (Some(url), None) if !url.is_empty() => {
                    format!(" ({})", clip(&display_url(url), 50))
                }
                _ => String::new(),
            };
            format!(
                "#{} {} {}{}",
                e["seq"].as_u64().unwrap_or(0),
                e["level"].as_str().unwrap_or("log"),
                clip(&one_line(e["text"].as_str().unwrap_or("")), 400),
                loc
            )
        })
        .collect();
    lines.push(format!(
        "[{} entries since attach · next: --since {}]",
        entries.len(),
        host["next_seq"].as_u64().unwrap_or(0)
    ));
    result.text = lines.join("\n");
    result.data = json!({ "tab": record.id(), "entries": entries, "next_seq": host["next_seq"], "since_attach": true });
    result
}

pub fn network_result(
    record: &BrowserTabRecord,
    page: &PageInfo,
    host: &Value,
) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let entries = host["entries"].as_array().cloned().unwrap_or_default();
    let mut lines: Vec<String> = entries
        .iter()
        .map(|e| {
            let status = if e["failed"].as_bool().unwrap_or(false) {
                format!("FAILED {}", e["failure"].as_str().unwrap_or(""))
            } else {
                e["status"]
                    .as_u64()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "…".into())
            };
            format!(
                "#{} {} {} {} {} {}ms",
                e["seq"].as_u64().unwrap_or(0),
                e["method"].as_str().unwrap_or("GET"),
                clip(e["url"].as_str().unwrap_or(""), 120),
                e["type"].as_str().unwrap_or(""),
                status,
                e["duration_ms"].as_u64().unwrap_or(0)
            )
        })
        .collect();
    lines.push(format!(
        "[{} requests since attach · next: --since {}]",
        entries.len(),
        host["next_seq"].as_u64().unwrap_or(0)
    ));
    result.text = lines.join("\n");
    result.data = json!({ "tab": record.id(), "entries": entries, "next_seq": host["next_seq"] });
    result
}

pub fn wait_result(record: &BrowserTabRecord, page: &PageInfo, host: &Value) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let matched = host["matched"].as_bool().unwrap_or(false);
    result.text = format!(
        "{} after {} ms",
        if matched { "matched" } else { "not matched" },
        host["elapsed_ms"].as_u64().unwrap_or(0)
    );
    result.data =
        json!({ "tab": record.id(), "matched": matched, "elapsed_ms": host["elapsed_ms"] });
    result
}

pub fn scroll_result(record: &BrowserTabRecord, page: &PageInfo, host: &Value) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    result.text = format!(
        "scroll_y {} of {}",
        host["scroll_y"].as_u64().unwrap_or(0),
        host["scroll_height"].as_u64().unwrap_or(0)
    );
    result.data = json!({ "tab": record.id(), "scroll_y": host["scroll_y"], "scroll_height": host["scroll_height"] });
    result
}

pub fn eval_result(
    record: &BrowserTabRecord,
    page: &PageInfo,
    host: &Value,
    max: usize,
) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let value = host["value"].clone();
    let rendered = match &value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    let paged = page_text(&sanitize(&rendered), 0, Some(max as u64));
    result.text = if paged.truncated {
        format!(
            "{}\n[{} of {} chars · --max N for more]",
            paged.text, paged.end, paged.total
        )
    } else {
        paged.text
    };
    result.data = json!({ "tab": record.id(), "value": value, "truncated": paged.truncated });
    result
}

/// An act's outcome: `clicked button "Merge" (e41)`, plus what changed.
pub fn act_result(
    record: &BrowserTabRecord,
    page: &PageInfo,
    kind: &str,
    target: &str,
    host: &Value,
    typed_len: Option<usize>,
) -> BrowserRunResult {
    let mut result = base(record, Some(page));
    let role = host["role"].as_str().unwrap_or("element");
    let name = clip(&one_line(host["name"].as_str().unwrap_or("")), 60);
    let element = if name.is_empty() {
        format!("{role} ({target})")
    } else {
        format!("{role} \"{name}\" ({target})")
    };
    let mut lines = vec![match (kind, typed_len) {
        ("click", _) => format!("clicked {element}"),
        ("hover", _) => format!("hovered {element}"),
        ("select", _) => format!("selected an option of {element}"),
        ("press", _) => format!("pressed a key on {element}"),
        ("fill", Some(len)) => format!("filled {len} chars into {element}"),
        (_, Some(len)) => format!("typed {len} chars into {element}"),
        _ => format!("{kind} on {element}"),
    }];
    if host["navigated"].as_bool().unwrap_or(false) {
        lines.push(format!(
            "navigated: {} → {}",
            display_url(host["url_before"].as_str().unwrap_or("")),
            display_url(&page.url)
        ));
    }
    if page.dialog_open {
        lines.push("a dialog opened: browser dialog accept|dismiss, or the user answers it".into());
    }
    result.text = lines.join("\n");
    result.data = json!({
        "tab": record.id(), "kind": kind, "target": target, "role": role, "name": name,
        "navigated": host["navigated"], "url": page.url, "title": page.title, "dialog_open": page.dialog_open,
        "typed_len": typed_len,
    });
    result
}

/// The ledger detail of an act: the element, never the text.
pub fn act_detail(kind: &str, target: &str, host: &Value, typed_len: Option<usize>) -> String {
    let role = host["role"].as_str().unwrap_or("element");
    let name = clip(&one_line(host["name"].as_str().unwrap_or("")), 40);
    let element = if name.is_empty() {
        format!("{role} {target}")
    } else {
        format!("{role} \"{name}\" {target}")
    };
    match typed_len {
        Some(len) => format!("{kind} {len} chars into {element}"),
        None => format!("{kind} {element}"),
    }
}

/// One line per batch step, then the optional final result.
pub struct BatchStepLine {
    pub index: usize,
    pub op: String,
    pub outcome: String,
    pub ok: bool,
    pub skipped: bool,
}

pub fn batch_result(
    header: String,
    tab: Option<String>,
    steps: &[BatchStepLine],
    final_result: Option<&BrowserRunResult>,
) -> BrowserRunResult {
    let mut lines: Vec<String> = steps
        .iter()
        .map(|step| {
            let mark = if step.skipped {
                "skipped"
            } else if step.ok {
                "ok"
            } else {
                "error"
            };
            format!(
                "{:>2}. {:<10} {mark:<7} {}",
                step.index + 1,
                step.op,
                sanitize(&step.outcome)
            )
        })
        .collect();
    let ok = steps.iter().filter(|s| s.ok).count();
    let failed = steps.iter().filter(|s| !s.ok && !s.skipped).count();
    let skipped = steps.iter().filter(|s| s.skipped).count();
    lines.push(format!(
        "[{} steps · {ok} ok · {failed} failed · {skipped} skipped]",
        steps.len()
    ));
    let mut data = json!({
        "steps": steps.iter().map(|s| json!({ "index": s.index, "op": s.op, "ok": s.ok, "skipped": s.skipped, "outcome": s.outcome })).collect::<Vec<_>>(),
        "ok": ok, "failed": failed, "skipped": skipped,
    });
    let mut result = BrowserRunResult {
        header,
        tab,
        ..Default::default()
    };
    if let Some(final_result) = final_result {
        lines.push(String::new());
        lines.push(final_result.header.clone());
        if !final_result.text.is_empty() {
            lines.push(final_result.text.clone());
        }
        data["final"] = final_result.data.clone();
        result.image_path = final_result.image_path.clone();
        result.image_inline_path = final_result.image_inline_path.clone();
        result.image_mime = final_result.image_mime.clone();
    }
    result.text = lines.join("\n");
    result.data = data;
    result
}

/// The tabs table: `*` marks the caller's current tab.
pub fn tabs_result(
    profile: &str,
    records: &[BrowserTabRecord],
    cursor: Option<&TabKey>,
    now: u64,
    active_seconds: u64,
) -> BrowserRunResult {
    let mut lines = Vec::new();
    for record in records {
        let mark = if cursor == Some(&record.key()) {
            "*"
        } else {
            " "
        };
        let who = match &record.last {
            Some(touch) => format!(
                "{} {} {}",
                touch.actor.label(),
                touch.op,
                age(now.saturating_sub(touch.at))
            ),
            None => format!("opened by {}", record.opened_by.label()),
        };
        let user = if record.last_actor.is_user() && record.last.is_some() {
            format!(" · user {}", age(now.saturating_sub(record.last_at)))
        } else {
            String::new()
        };
        let flags = [
            record.selected.then_some("selected"),
            record.dialog_open.then_some("dialog"),
            (!record.last_actor.is_user() && now.saturating_sub(record.last_at) <= active_seconds)
                .then_some("active"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(",");
        lines.push(format!(
            "{mark}{:<4} {:<50} {:<40} {}{}{}",
            record.short,
            clip(&display_url(&record.url), 50),
            clip(&one_line(&record.title), 40),
            who,
            user,
            if flags.is_empty() {
                String::new()
            } else {
                format!(" [{flags}]")
            }
        ));
    }
    if lines.is_empty() {
        lines.push(format!(
            "no open tabs in profile {profile}; browser open <url>"
        ));
    }
    BrowserRunResult {
        header: format!("[{profile} · {} tabs]", records.len()),
        text: lines.join("\n"),
        tab: cursor
            .and_then(|key| records.iter().find(|r| &r.key() == key))
            .map(BrowserTabRecord::id),
        data: json!({
            "profile": profile,
            "tabs": records.iter().map(|record| json!({
                "id": record.id(), "url": record.url, "title": record.title, "selected": record.selected,
                "current": cursor == Some(&record.key()),
                "opened_by": record.opened_by, "last": record.last, "last_actor": record.last_actor,
                "users": record.users, "dialog_open": record.dialog_open,
            })).collect::<Vec<_>>(),
        }),
        ..Default::default()
    }
}

/// `12s`, `3m`, `2h`, `4d`.
pub fn age(seconds: u64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::BrowserActor;
    use crate::browser::state::TabState;

    fn record() -> BrowserTabRecord {
        BrowserTabRecord {
            short: "t3".into(),
            profile: "main".into(),
            target_id: "T".into(),
            url: "https://github.com/foo/bar/pull/412".into(),
            title: "fix: the thing · PR #412".into(),
            selected: true,
            opened_by: BrowserActor::User,
            opened_at: 0,
            last: None,
            last_actor: BrowserActor::User,
            last_at: 0,
            users: vec![],
            dialog_open: false,
            console_errors: 0,
            state: TabState::Open,
            closed_at: None,
        }
    }

    #[test]
    fn header_names_tab_status_url_title_and_dialog() {
        let page = PageInfo {
            url: String::new(),
            title: String::new(),
            dialog_open: true,
            status: Some(200),
        };
        assert_eq!(
            header(&record(), Some(&page)),
            "[main:t3 · 200 · github.com/foo/bar/pull/412 · \"fix: the thing · PR #412\" · dialog open]"
        );
        assert_eq!(
            header(&record(), None),
            "[main:t3 · github.com/foo/bar/pull/412 · \"fix: the thing · PR #412\"]"
        );
    }

    #[test]
    fn page_text_reaches_the_terminal_without_escapes() {
        let mut r = record();
        r.title = "PR \u{1b}]52;c;ZXZpbA==\u{7}#412\u{9b}x".into();
        r.url = "https://a/\u{1b}[2J".into();
        let header = header(&r, None);
        assert!(
            !header.contains('\u{1b}') && !header.contains('\u{7}') && !header.contains('\u{9b}'),
            "{header:?}"
        );
        assert!(header.contains("PR ]52;c;ZXZpbA==#412x"), "{header}");
        let page = PageInfo::default();
        let paged = page_text("line\u{1b}[31mred\u{7}\n\tkeep", 0, None);
        let read = read_result(&r, &page, "markdown", &paged, None, None);
        assert!(
            read.text.starts_with("line[31mred\n\tkeep"),
            "{:?}",
            read.text
        );
        assert_eq!(sanitize("plain"), "plain");
        let table = tabs_result("main", &[r.clone()], None, 0, 120);
        assert!(!table.text.contains('\u{1b}'));
        let ev = eval_result(&r, &page, &json!({ "value": "\u{1b}]0;t\u{7}v" }), 100);
        assert_eq!(ev.text, "]0;tv");
    }

    #[test]
    fn urls_normalize_and_bad_schemes_are_refused() {
        assert_eq!(
            normalize_url("example.com/x").unwrap(),
            "https://example.com/x"
        );
        assert_eq!(normalize_url(" https://a/ ").unwrap(), "https://a/");
        assert_eq!(normalize_url("about:blank").unwrap(), "about:blank");
        assert_eq!(normalize_url("ABOUT:BLANK").unwrap(), "ABOUT:BLANK");
        assert!(normalize_url("about:settings").is_err());
        assert!(normalize_url("about:blank#x").is_err());
        assert!(normalize_url("javascript:alert(1)").is_err());
        assert!(normalize_url("ftp://x").is_err());
        assert!(normalize_url("").is_err());
        assert_eq!(read_format(None).unwrap(), "markdown");
        assert_eq!(read_format(Some("aria")).unwrap(), "snapshot");
        assert!(read_format(Some("pdf")).is_err());
    }

    #[test]
    fn paging_math_and_footers() {
        let content: String = (0..100)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let first = page_text(&content, 0, Some(40));
        assert_eq!(
            (first.offset, first.end, first.total, first.truncated),
            (0, 40, 100, true)
        );
        assert_eq!(
            first.footer("browser read t3"),
            "[chars 0–40 of 100 · next: browser read t3 --offset 40]"
        );
        assert_eq!(first.detail(), "0–40/100");
        let last = page_text(&content, 80, Some(40));
        assert_eq!((last.end, last.truncated), (100, false));
        assert_eq!(last.footer("x"), "[chars 80–100 of 100 · end]");
        let all = page_text(&content, 0, None);
        assert_eq!(all.footer("x"), "[100 chars]");
        assert_eq!(all.detail(), "100");
        let past = page_text(&content, 500, Some(10));
        assert_eq!((past.offset, past.end, past.text.len()), (100, 100, 0));
        // multi-byte safe
        let emoji = "héllo wörld ✓ done";
        let p = page_text(emoji, 6, Some(5));
        assert_eq!(p.text, "wörld");
    }

    #[test]
    fn find_gives_char_offsets_and_windows() {
        let content = "Intro text here.\n\n## Merge\n\nPress the Merge pull request button now. Another merge later.";
        let matches = find_matches(content, "merge", 10, 20).unwrap();
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].offset, 21);
        assert!(
            matches[1].context.contains("Merge pull"),
            "{}",
            matches[1].context
        );
        assert!(matches[0].context.starts_with('…') && matches[0].context.ends_with('…'));
        let regex = find_matches(content, "/m\\w+ge/", 10, 20).unwrap();
        assert_eq!(regex.len(), 3);
        assert!(find_matches(content, "/[/", 10, 20).is_err());
        assert_eq!(find_matches(content, "zzz", 10, 20).unwrap().len(), 0);
        assert_eq!(
            find_matches("ünïcode ünïcode", "ünï", 10, 4).unwrap()[1].offset,
            8
        );
        assert_eq!(find_matches(content, "merge", 1, 20).unwrap().len(), 1);
    }

    #[test]
    fn open_card_and_tabs_table_render() {
        let page = PageInfo {
            url: "https://a/".into(),
            title: "A".into(),
            dialog_open: false,
            status: Some(200),
        };
        let host = json!({ "card": { "headings": 3, "links": 12, "forms": 1, "text_chars": 4800, "main_chars": 2100, "login_wall": true, "preview": "# Hello\n\nbody" } });
        let result = open_result(&record(), &page, &host);
        assert!(result
            .text
            .starts_with("headings 3 · links 12 · forms 1 · text ≈ 4k · main ≈ 2k"));
        assert!(result.text.contains("login wall?"));
        assert!(result.text.contains("# Hello"));
        assert!(result.text.contains("[more: browser read]"));
        assert_eq!(result.tab.as_deref(), Some("main:t3"));

        let mut r = record();
        r.last = Some(crate::api::schema::BrowserTouch {
            actor: BrowserActor::User,
            op: "read".into(),
            detail: String::new(),
            at: 90,
            ok: true,
        });
        let table = tabs_result("main", &[r.clone()], Some(&r.key()), 100, 120);
        assert_eq!(table.header, "[main · 1 tabs]");
        assert!(table.text.starts_with("*t3  "), "{}", table.text);
        assert!(table.text.contains("you read 10s"));
        assert!(table.text.contains("[selected]"));
        let empty = tabs_result("main", &[], None, 0, 120);
        assert!(empty.text.contains("no open tabs"));
        assert_eq!(age(12), "12s");
        assert_eq!(age(3 * 60 + 5), "3m");
        assert_eq!(age(2 * 3600), "2h");
        assert_eq!(age(3 * 86_400), "3d");
    }

    #[test]
    fn eval_truncates_and_read_footers_carry_the_format() {
        let page = PageInfo::default();
        let result = eval_result(&record(), &page, &json!({ "value": "x".repeat(300) }), 100);
        assert!(result
            .text
            .ends_with("[100 of 300 chars · --max N for more]"));
        assert_eq!(result.data["truncated"], true);
        let paged = page_text("abc", 0, Some(2));
        let read = read_result(&record(), &page, "snapshot", &paged, Some("e4"), None);
        assert!(
            read.text
                .ends_with("next: browser read t3 --format snapshot --ref e4 --offset 2]"),
            "{}",
            read.text
        );
        assert_eq!(read.data["next_offset"], 2);
    }
}
