//! `herdr agent notify` (fork): show the user an agent card from the pane
//! this command runs in (`agent.notify`). The sender is that pane's own
//! record on the server; nothing on the command line names it.

use crate::api::schema::{AgentNoticeKind, AgentNotifyParams, Method, Request};

pub(crate) const USAGE: &str = "\
usage: herdr agent notify <title> [--body TEXT] [--kind info|question|done|warning] [--json]

Shows your user a card from this pane's agent until they dismiss it or visit
its tab. Run it inside a herdr pane. Rate-limited per agent.";

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotifyArgs {
    title: String,
    body: Option<String>,
    kind: AgentNoticeKind,
    json: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Help,
    Run(NotifyArgs),
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut title: Option<String> = None;
    let mut body = None;
    let mut kind = AgentNoticeKind::Info;
    let mut json = false;
    let mut rest = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if rest {
            title = Some(match title {
                Some(title) => format!("{title} {arg}"),
                None => arg.clone(),
            });
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" | "help" if title.is_none() => return Ok(Parsed::Help),
            "--" => rest = true,
            "--json" if inline.is_none() => json = true,
            "--body" | "--kind" => {
                let value = match inline {
                    Some(value) => value,
                    None => args
                        .next()
                        .cloned()
                        .ok_or_else(|| format!("{flag} needs a value"))?,
                };
                if flag == "--body" {
                    body = Some(value);
                } else {
                    kind = AgentNoticeKind::parse(&value).ok_or_else(|| {
                        format!("--kind {value:?}: one of info, question, done, warning")
                    })?;
                }
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option {other}"));
            }
            _ => {
                if title.is_some() {
                    return Err("one title (quote it)".into());
                }
                title = Some(arg.clone());
            }
        }
    }
    let title = title
        .filter(|title| !title.trim().is_empty())
        .ok_or_else(|| "a title is required".to_string())?;
    Ok(Parsed::Run(NotifyArgs {
        title,
        body,
        kind,
        json,
    }))
}

/// `herdr agent notify …`: 0 shown or deduped, 1 refused by the server,
/// 2 usage or not inside a herdr pane.
pub fn run(args: &[String]) -> std::io::Result<i32> {
    let parsed = match parse(args) {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return Ok(0);
        }
        Ok(Parsed::Run(parsed)) => parsed,
        Err(message) => {
            eprintln!("herdr agent notify: {message}\n{USAGE}");
            return Ok(2);
        }
    };
    let Some(caller_pane) = super::target::caller_pane_id() else {
        eprintln!("herdr agent notify: run inside a herdr pane (HERDR_PANE_ID is not set)");
        return Ok(2);
    };
    let response = super::send_request(&Request {
        id: "cli:agent:notify".into(),
        method: Method::AgentNotify(AgentNotifyParams {
            caller_pane,
            kind: parsed.kind,
            title: parsed.title,
            body: parsed.body,
        }),
    })?;
    if parsed.json {
        return super::print_response(&response);
    }
    if let Some(error) = response.get("error") {
        eprintln!(
            "herdr agent notify: {}: {}",
            error["code"].as_str().unwrap_or("error"),
            error["message"].as_str().unwrap_or("")
        );
        return Ok(1);
    }
    let id = response
        .pointer("/result/id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    match response
        .pointer("/result/outcome")
        .and_then(serde_json::Value::as_str)
    {
        Some("deduped") => println!("deduped: card {id} refreshed"),
        _ => println!("shown: card {id}"),
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn parses_title_body_kind_and_json() {
        assert_eq!(
            parse(&args(&[
                "build done",
                "--kind",
                "done",
                "--body=all green",
                "--json"
            ])),
            Ok(Parsed::Run(NotifyArgs {
                title: "build done".into(),
                body: Some("all green".into()),
                kind: AgentNoticeKind::Done,
                json: true,
            }))
        );
        assert_eq!(
            parse(&args(&["--kind=question", "--", "--weird", "title"])),
            Ok(Parsed::Run(NotifyArgs {
                title: "--weird title".into(),
                body: None,
                kind: AgentNoticeKind::Question,
                json: false,
            }))
        );
        assert_eq!(parse(&args(&["--help"])), Ok(Parsed::Help));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["  "])).is_err());
        assert!(parse(&args(&["a", "b"])).is_err());
        assert!(parse(&args(&["a", "--kind", "loud"])).is_err());
        assert!(parse(&args(&["a", "--body"])).is_err());
        assert!(parse(&args(&["a", "--from", "w1:p1"])).is_err());
    }
}
