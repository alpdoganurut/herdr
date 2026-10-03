//! `herdr tab closed` and `herdr tab reopen`: the recently closed agent
//! sessions (`session.closed_list`, `session.closed_reopen`).

use crate::api::schema::{
    compact_closed_age, ClosedSessionInfo, ClosedSessionTarget, EmptyParams, Method, Request,
};

const CLOSED_USAGE: &str = "usage: herdr tab closed [--json]";
const REOPEN_USAGE: &str = "usage: herdr tab reopen <n|id|session-id-prefix>";

/// The closed sessions, newest first, from the server.
fn fetch_closed_sessions(
    request_id: &str,
) -> std::io::Result<Result<Vec<ClosedSessionInfo>, serde_json::Value>> {
    let response = super::send_request(&Request {
        id: request_id.into(),
        method: Method::SessionClosedList(EmptyParams::default()),
    })?;
    if response.get("error").is_some() {
        return Ok(Err(response));
    }
    let sessions = response
        .pointer("/result/sessions")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    serde_json::from_value(sessions)
        .map(Ok)
        .map_err(std::io::Error::other)
}

pub(super) fn tab_closed(args: &[String]) -> std::io::Result<i32> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "help" | "--help" | "-h" => {
                eprintln!("{CLOSED_USAGE}");
                return Ok(0);
            }
            _ => {
                eprintln!("{CLOSED_USAGE}");
                return Ok(2);
            }
        }
    }
    let sessions = match fetch_closed_sessions("cli:tab:closed")? {
        Ok(sessions) => sessions,
        Err(response) => return super::print_response(&response),
    };
    if json {
        let value = serde_json::json!({ "sessions": sessions });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(0);
    }
    println!("{}", format_closed_sessions(&sessions, unix_now()));
    Ok(0)
}

pub(super) fn tab_reopen(args: &[String]) -> std::io::Result<i32> {
    let [query] = args else {
        eprintln!("{REOPEN_USAGE}");
        return Ok(2);
    };
    if matches!(query.as_str(), "help" | "--help" | "-h") {
        eprintln!("{REOPEN_USAGE}");
        return Ok(0);
    }
    let sessions = match fetch_closed_sessions("cli:tab:reopen")? {
        Ok(sessions) => sessions,
        Err(response) => return super::print_response(&response),
    };
    let entry = match resolve_closed_session(&sessions, query) {
        Ok(entry) => entry,
        Err(message) => {
            eprintln!("{message}");
            return Ok(1);
        }
    };
    // Fork (agents v2): an agent reopens through the check.
    match super::agent_route::route()? {
        Err(code) => return Ok(code),
        Ok(super::agent_route::Route::Agent(pane)) => {
            return super::agent_route::call(
                "cli:agents.reopen_tab",
                Method::AgentsReopenTab(crate::api::schema::agents_model::AgentsReopenTabParams {
                    caller_pane: pane,
                    closed_id: entry.id.clone(),
                }),
            )
        }
        Ok(super::agent_route::Route::User | super::agent_route::Route::OldServer) => {}
    }
    super::print_response(&super::send_request(&Request {
        id: "cli:tab:reopen".into(),
        method: Method::SessionClosedReopen(ClosedSessionTarget {
            id: entry.id.clone(),
        }),
    })?)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// Resolve `query` to one entry: a 1-based list number, an entry id, or a
/// unique prefix of a session id.
fn resolve_closed_session<'a>(
    sessions: &'a [ClosedSessionInfo],
    query: &str,
) -> Result<&'a ClosedSessionInfo, String> {
    if sessions.is_empty() {
        return Err("no closed sessions".into());
    }
    if let Some(entry) = sessions.iter().find(|entry| entry.id == query) {
        return Ok(entry);
    }
    if !query.is_empty() && query.bytes().all(|byte| byte.is_ascii_digit()) {
        if let Some(entry) = query
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|index| sessions.get(index))
        {
            return Ok(entry);
        }
    }
    let matches: Vec<&ClosedSessionInfo> = sessions
        .iter()
        .filter(|entry| !query.is_empty() && entry.session_id.starts_with(query))
        .collect();
    match matches[..] {
        [entry] => Ok(entry),
        [] => Err(format!(
            "no closed session matches {query:?}; see `herdr tab closed`"
        )),
        _ => Err(format!(
            "{query:?} matches {} closed sessions; use the number from `herdr tab closed`",
            matches.len()
        )),
    }
}

fn format_closed_sessions(sessions: &[ClosedSessionInfo], now: u64) -> String {
    if sessions.is_empty() {
        return "no closed sessions".into();
    }
    let rows: Vec<[String; 5]> = sessions
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            [
                (index + 1).to_string(),
                entry.title().to_string(),
                entry.space_name.clone(),
                entry.dir_name().to_string(),
                compact_closed_age(now.saturating_sub(entry.closed_at)),
            ]
        })
        .collect();
    let header = ["#", "LABEL", "GROUP", "DIR", "CLOSED"];
    let widths: Vec<usize> = (0..header.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .chain(std::iter::once(header[column].len()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: [&str; 5]| {
        let mut out = format!("{:>width$}", cells[0], width = widths[0]);
        for (column, cell) in cells.iter().enumerate().skip(1) {
            out.push_str("  ");
            if column + 1 == cells.len() {
                out.push_str(cell);
            } else {
                let pad = widths[column].saturating_sub(cell.chars().count());
                out.push_str(cell);
                out.push_str(&" ".repeat(pad));
            }
        }
        out
    };
    let mut out = line(header);
    for row in &rows {
        out.push('\n');
        out.push_str(&line([
            row[0].as_str(),
            row[1].as_str(),
            row[2].as_str(),
            row[3].as_str(),
            row[4].as_str(),
        ]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_resume::AgentSessionRefKind;

    fn entry(session_id: &str, label: Option<&str>, closed_at: u64) -> ClosedSessionInfo {
        ClosedSessionInfo {
            id: format!("{closed_at}-{session_id}"),
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref_kind: AgentSessionRefKind::Id,
            session_id: session_id.into(),
            transcript_path: None,
            label: label.map(str::to_string),
            color: None,
            important: false,
            remind_every: None,
            space_id: "w_1".into(),
            space_name: "leap".into(),
            cwd: "/home/me/src/herdr".into(),
            closed_at,
        }
    }

    fn sessions() -> Vec<ClosedSessionInfo> {
        vec![
            entry("abc123", Some("review"), 1_000),
            entry("abd456", None, 900),
            entry("f00", Some("12"), 800),
        ]
    }

    #[test]
    fn reopen_resolves_numbers_ids_and_unique_session_prefixes() {
        let sessions = sessions();
        let pick = |query: &str| resolve_closed_session(&sessions, query).map(|e| e.id.clone());
        assert_eq!(pick("1").unwrap(), "1000-abc123");
        assert_eq!(pick("3").unwrap(), "800-f00");
        assert_eq!(pick("900-abd456").unwrap(), "900-abd456");
        assert_eq!(pick("abc").unwrap(), "1000-abc123");
        assert_eq!(pick("f0").unwrap(), "800-f00");
        assert!(pick("ab")
            .unwrap_err()
            .contains("matches 2 closed sessions"));
        assert!(pick("zzz")
            .unwrap_err()
            .contains("no closed session matches"));
        assert!(pick("4").unwrap_err().contains("no closed session matches"));
        assert!(pick("").is_err());
        assert_eq!(
            resolve_closed_session(&[], "1").unwrap_err(),
            "no closed sessions"
        );
    }

    #[test]
    fn closed_list_is_numbered_with_label_group_dir_and_age() {
        let text = format_closed_sessions(&sessions(), 1_000 + 2 * 3_600);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "#  LABEL   GROUP  DIR    CLOSED");
        assert_eq!(lines[1], "1  review  leap   herdr  2h ago");
        assert_eq!(lines[2], "2  claude  leap   herdr  2h ago");
        assert_eq!(lines[3], "3  12      leap   herdr  2h ago");
        assert_eq!(format_closed_sessions(&[], 0), "no closed sessions");
    }

    #[test]
    fn tab_closed_and_reopen_parse_their_arguments() {
        assert_eq!(tab_closed(&["--bogus".into()]).unwrap(), 2);
        assert_eq!(tab_closed(&["--help".into()]).unwrap(), 0);
        assert_eq!(tab_reopen(&[]).unwrap(), 2);
        assert_eq!(tab_reopen(&["1".into(), "2".into()]).unwrap(), 2);
        assert_eq!(tab_reopen(&["--help".into()]).unwrap(), 0);
    }
}
