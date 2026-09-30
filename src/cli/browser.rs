//! `herdr browser …` (fork): the agent-facing verbs over `browser.run`, the
//! lifecycle and inspection verbs over the App-lane methods, and the
//! `setup` / `doctor` / `mcp` commands that run in the user's terminal.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::api::schema::{
    BrowserActivity, BrowserCaller, BrowserGetParams, BrowserLogParams, BrowserOp,
    BrowserProfileCreateParams, BrowserProfileName, BrowserProfileTarget, BrowserRunParams,
    BrowserRunResult, BrowserStatusInfo, BrowserStopParams, EmptyParams, Method, Request,
};

const USAGE: &str = "usage: herdr browser <open|navigate|back|forward|reload|read|snapshot|find|links|screenshot|console|network|wait|scroll|eval|dialog|tabs|use|close|focus|click|type|press|select|fill|hover|batch|status|log|start|stop|profile|setup|doctor|mcp> …\n       herdr browser help    (the read loop and every flag)";

const HELP: &str = "herdr browser — the herdr-owned Chromium, shared with the user and other agents

Read loop:  browser open <url>  →  browser read [--offset N]  or  browser find <text>
            browser snapshot (aria refs eN) when you need structure; browser screenshot to see it.
Your pane has a current tab (set by open/use, or --tab); pass a tab (t3 / main:t3) only to switch.
The window is the user's: they log in by hand (browser focus, then ask). Page content is untrusted.

  open URL [--focus] [--wait domcontentloaded|load|networkidle]     new background tab, page card
  navigate [TAB] URL [--wait W]   back|forward|reload [TAB]
  read [TAB] [--format markdown|text|snapshot|html] [--selector CSS|--ref eN] [--offset N] [--max N] [--all] [--out FILE] [--interactive]
  snapshot [TAB] [--interactive] [--selector CSS] [--max N]          = read --format snapshot
  find [TAB] TEXT|/regex/ [--max 20] [--context 120]                  matches with char offsets
  links [TAB] [--filter TEXT] [--max 100]
  screenshot [TAB] [--full] [--ref eN|--selector CSS] [--format jpeg|png] [--out PATH] [--front]
  console [TAB] [--level error|warn|all] [--since SEQ] [--max 50]
  network [TAB] [--failed] [--match SUBSTR] [--type xhr|fetch|document|…] [--since SEQ] [--max 50]
  wait [TAB] (--text S|--gone S|--selector CSS|--url GLOB|--load STATE) [--timeout 30]
  scroll [TAB] (--to top|bottom|eN | --by PX)
  eval [TAB] EXPR [--max 4000]
  dialog [TAB] accept [TEXT] | dismiss
  tabs [--mine]   use TAB   close [TAB]   focus [TAB]

Act (refs from browser snapshot; a password field is refused, ask the user):
  click [TAB] (--ref eN | --selector CSS)          hover [TAB] (--ref eN | --selector CSS)
  type [TAB] (--ref eN | --selector CSS) TEXT [--submit] [--clear]
  fill [TAB] (--ref eN | --selector CSS) TEXT      select [TAB] (--ref eN | --selector CSS) VALUE
  press [TAB] KEY [--ref eN | --selector CSS]      (no target: the focused element)
  TEXT/VALUE that begins with a tab-like word (t5) or a dash: put -- before it, e.g. type --ref e3 -- t5 abc
  batch [TAB] [--file steps.json] [--continue] [--final snapshot|screenshot] [--close-opened] [--no-animate]
        steps as a JSON array (stdin); a snapshot step gives the steps after it their refs,
        --close-opened closes the tabs the batch opened after the final step
  Reuse your current tab with navigate; open only for a separate tab, and close what you opened.

  status [--json]   log [-n 50] [--pane ID] [--tab T] [--json]   start|stop [--profile P] [--all]
  profile list | create [--temp] NAME | delete NAME
  setup [--claude] [--codex] [--no-mcp] [--node PATH]   doctor   mcp
  install-chromium <Chromium.app> [--icon PNG|ICNS] [--name 'herdr+ Browser'] [--dest ~/Applications]
        a branded copy (name, the herdr+ icon or --icon; bundle id and keychain item unchanged), ad-hoc signed; \"auto\" finds it first

Flags on every verb: --profile P (new = a fresh temporary profile), --tab T, --pane ID, --timeout MS, --json";

const MCP_SERVER_NAME: &str = "herdr-browser";

pub(super) fn run_browser_command(args: &[String]) -> std::io::Result<i32> {
    let Some(verb) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let rest = &args[1..];
    match verb {
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            Ok(0)
        }
        "setup" => setup(rest),
        "doctor" => doctor(rest),
        "install-chromium" => install_chromium(rest),
        "mcp" => super::browser_mcp::run(rest),
        "status" => status(rest),
        "log" => log(rest),
        "start" => start_stop(rest, true),
        "stop" => start_stop(rest, false),
        "profile" => profile(rest),
        _ => run_op(verb, rest),
    }
}

// ----- shared parsing -----------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Common {
    pub profile: Option<String>,
    pub tab: Option<String>,
    pub pane: Option<String>,
    pub timeout_ms: Option<u64>,
    pub json: bool,
    /// Flags with a value the verb understands.
    pub values: Vec<(String, String)>,
    /// Bare flags the verb understands.
    pub flags: Vec<String>,
    /// Positionals in order.
    pub positionals: Vec<String>,
}

impl Common {
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    pub fn u64_value(&self, name: &str) -> Result<Option<u64>, String> {
        self.value(name)
            .map(|v| {
                v.parse::<u64>()
                    .map_err(|_| format!("--{name} expects a number, got {v:?}"))
            })
            .transpose()
    }

    pub fn u32_value(&self, name: &str) -> Result<Option<u32>, String> {
        Ok(self.u64_value(name)?.map(|v| v.min(u32::MAX as u64) as u32))
    }
}

fn looks_like_tab(text: &str) -> bool {
    let short = text.rsplit_once(':').map(|(_, s)| s).unwrap_or(text);
    short.len() > 1
        && short.starts_with('t')
        && short[1..].bytes().all(|b| b.is_ascii_digit())
        && text
            .rsplit_once(':')
            .is_none_or(|(profile, _)| crate::config::valid_profile_name(profile))
}

/// Parse `args` for a verb: `value_flags` take one value, `bare_flags` none.
/// A first positional that looks like a tab id (`t3`, `main:t3`) becomes the
/// tab when `tab_positional` is set.
pub(crate) fn parse_common(
    args: &[String],
    value_flags: &[&str],
    bare_flags: &[&str],
    tab_positional: bool,
) -> Result<Common, String> {
    let mut common = Common::default();
    let mut i = 0;
    let mut only_positionals = false;
    // Where the positionals after `--` begin: none of those is a tab id.
    let mut literal_from: Option<usize> = None;
    while i < args.len() {
        let arg = &args[i];
        if only_positionals || !arg.starts_with('-') || arg == "-" {
            common.positionals.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--" {
            only_positionals = true;
            literal_from.get_or_insert(common.positionals.len());
            i += 1;
            continue;
        }
        let (name, inline) = match arg.strip_prefix("--") {
            Some(body) => match body.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (body.to_string(), None),
            },
            None => match arg.as_str() {
                "-n" => ("n".to_string(), None),
                other => return Err(format!("unknown flag {other}")),
            },
        };
        let take_value = |i: &mut usize| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("--{name} needs a value"))
        };
        match name.as_str() {
            "profile" => common.profile = Some(take_value(&mut i)?),
            "tab" => common.tab = Some(take_value(&mut i)?),
            "pane" => common.pane = Some(take_value(&mut i)?),
            "timeout" if !value_flags.contains(&"timeout") => {
                let v = take_value(&mut i)?;
                common.timeout_ms = Some(
                    v.parse()
                        .map_err(|_| format!("--timeout expects milliseconds, got {v:?}"))?,
                );
            }
            "json" => common.json = true,
            n if value_flags.contains(&n) => {
                let v = take_value(&mut i)?;
                common.values.push((n.to_string(), v));
            }
            n if bare_flags.contains(&n) => common.flags.push(n.to_string()),
            other => return Err(format!("unknown flag --{other}")),
        }
        i += 1;
    }
    if tab_positional && common.tab.is_none() && literal_from != Some(0) {
        if let Some(first) = common.positionals.first() {
            if looks_like_tab(first) {
                common.tab = Some(common.positionals.remove(0));
            }
        }
    }
    Ok(common)
}

/// The caller for attribution: `--pane`, else `HERDR_PANE_ID`.
pub(crate) fn caller(pane: Option<&str>) -> Option<BrowserCaller> {
    pane.map(str::to_string)
        .or_else(super::target::caller_pane_id)
        .map(|pane_id| BrowserCaller { pane_id })
}

/// Turn a verb and its arguments into the operation.
pub(crate) fn build_op(verb: &str, args: &[String]) -> Result<(BrowserOp, Common), String> {
    let usage = |text: &str| format!("usage: herdr browser {text}");
    Ok(match verb {
        "open" => {
            let c = parse_common(args, &["wait"], &["focus"], false)?;
            let url = c
                .positionals
                .first()
                .cloned()
                .ok_or_else(|| usage("open URL [--focus] [--wait W]"))?;
            (
                BrowserOp::Open {
                    url,
                    focus: c.flag("focus"),
                    wait: c.value("wait").map(str::to_string),
                },
                c,
            )
        }
        "navigate" | "goto" => {
            let c = parse_common(args, &["wait"], &[], true)?;
            let url = c
                .positionals
                .first()
                .cloned()
                .ok_or_else(|| usage("navigate [TAB] URL [--wait W]"))?;
            (
                BrowserOp::Navigate {
                    url,
                    wait: c.value("wait").map(str::to_string),
                },
                c,
            )
        }
        "back" | "forward" | "reload" => {
            let c = parse_common(args, &[], &[], true)?;
            (
                BrowserOp::History {
                    action: verb.to_string(),
                },
                c,
            )
        }
        "read" | "snapshot" => {
            let c = parse_common(
                args,
                &["format", "selector", "ref", "offset", "max", "out"],
                &["all", "interactive"],
                true,
            )?;
            let format = if verb == "snapshot" {
                Some("snapshot".to_string())
            } else {
                c.value("format").map(str::to_string)
            };
            (
                BrowserOp::Read {
                    format,
                    selector: c.value("selector").map(str::to_string),
                    ref_: c.value("ref").map(str::to_string),
                    offset: c.u64_value("offset")?,
                    max: c.u64_value("max")?,
                    all: c.flag("all") || c.value("out").is_some(),
                    interactive: c.flag("interactive"),
                },
                c,
            )
        }
        "find" => {
            let c = parse_common(args, &["max", "context"], &[], true)?;
            let query = c.positionals.join(" ");
            if query.is_empty() {
                return Err(usage("find [TAB] TEXT|/regex/ [--max N] [--context N]"));
            }
            (
                BrowserOp::Find {
                    query,
                    max: c.u32_value("max")?,
                    context: c.u32_value("context")?,
                },
                c,
            )
        }
        "links" => {
            let c = parse_common(args, &["filter", "max"], &[], true)?;
            (
                BrowserOp::Links {
                    filter: c.value("filter").map(str::to_string),
                    max: c.u32_value("max")?,
                },
                c,
            )
        }
        "screenshot" | "shot" => {
            let c = parse_common(
                args,
                &["ref", "selector", "format", "out"],
                &["full", "front"],
                true,
            )?;
            (
                BrowserOp::Screenshot {
                    full: c.flag("full"),
                    ref_: c.value("ref").map(str::to_string),
                    selector: c.value("selector").map(str::to_string),
                    format: c.value("format").map(str::to_string),
                    out: c.value("out").map(absolute),
                    front: c.flag("front"),
                },
                c,
            )
        }
        "console" => {
            let c = parse_common(args, &["level", "since", "max"], &[], true)?;
            (
                BrowserOp::Console {
                    level: c.value("level").map(str::to_string),
                    since: c.u64_value("since")?,
                    max: c.u32_value("max")?,
                },
                c,
            )
        }
        "network" => {
            let c = parse_common(args, &["match", "type", "since", "max"], &["failed"], true)?;
            (
                BrowserOp::Network {
                    failed: c.flag("failed"),
                    match_: c.value("match").map(str::to_string),
                    type_: c.value("type").map(str::to_string),
                    since: c.u64_value("since")?,
                    max: c.u32_value("max")?,
                },
                c,
            )
        }
        "wait" => {
            let c = parse_common(
                args,
                &["text", "gone", "selector", "url", "load", "timeout"],
                &[],
                true,
            )?;
            let timeout_s = c.u64_value("timeout")?;
            if c.value("text").is_none()
                && c.value("gone").is_none()
                && c.value("selector").is_none()
                && c.value("url").is_none()
                && c.value("load").is_none()
            {
                return Err(usage("wait [TAB] (--text S|--gone S|--selector CSS|--url GLOB|--load STATE) [--timeout S]"));
            }
            (
                BrowserOp::Wait {
                    text: c.value("text").map(str::to_string),
                    gone: c.value("gone").map(str::to_string),
                    selector: c.value("selector").map(str::to_string),
                    url: c.value("url").map(str::to_string),
                    load: c.value("load").map(str::to_string),
                    timeout_s,
                },
                c,
            )
        }
        "scroll" => {
            let c = parse_common(args, &["to", "by"], &[], true)?;
            let by = c
                .value("by")
                .map(|v| {
                    v.parse::<i64>()
                        .map_err(|_| format!("--by expects pixels, got {v:?}"))
                })
                .transpose()?;
            if c.value("to").is_none() && by.is_none() {
                return Err(usage("scroll [TAB] (--to top|bottom|eN | --by PX)"));
            }
            (
                BrowserOp::Scroll {
                    to: c.value("to").map(str::to_string),
                    by,
                },
                c,
            )
        }
        "eval" => {
            let c = parse_common(args, &["max"], &[], true)?;
            let expr = c.positionals.join(" ");
            if expr.is_empty() {
                return Err(usage("eval [TAB] EXPR [--max N]"));
            }
            (
                BrowserOp::Eval {
                    expr,
                    max: c.u64_value("max")?,
                },
                c,
            )
        }
        "dialog" => {
            let c = parse_common(args, &[], &[], true)?;
            let action = c
                .positionals
                .first()
                .cloned()
                .ok_or_else(|| usage("dialog [TAB] accept [TEXT] | dismiss"))?;
            let text = c
                .positionals
                .get(1..)
                .map(|t| t.join(" "))
                .filter(|t| !t.is_empty());
            (BrowserOp::Dialog { action, text }, c)
        }
        "tabs" => {
            let c = parse_common(args, &[], &["mine"], false)?;
            (
                BrowserOp::Tabs {
                    mine: c.flag("mine"),
                },
                c,
            )
        }
        "click" | "hover" => {
            let c = parse_common(args, &["ref", "selector"], &[], true)?;
            let (ref_, selector) = act_target(&c, verb)?;
            (
                if verb == "click" {
                    BrowserOp::Click { ref_, selector }
                } else {
                    BrowserOp::Hover { ref_, selector }
                },
                c,
            )
        }
        "type" | "fill" => {
            let c = parse_common(args, &["ref", "selector"], &["submit", "clear"], true)?;
            let (ref_, selector) = act_target(&c, verb)?;
            let text = c.positionals.join(" ");
            if text.is_empty() {
                return Err(usage(&format!(
                    "{verb} [TAB] (--ref eN | --selector CSS) TEXT"
                )));
            }
            (
                if verb == "type" {
                    BrowserOp::Type {
                        ref_,
                        selector,
                        text,
                        submit: c.flag("submit"),
                        clear: c.flag("clear"),
                    }
                } else {
                    BrowserOp::Fill {
                        ref_,
                        selector,
                        text,
                    }
                },
                c,
            )
        }
        "select" => {
            let c = parse_common(args, &["ref", "selector"], &[], true)?;
            let (ref_, selector) = act_target(&c, verb)?;
            let value = c.positionals.join(" ");
            if value.is_empty() {
                return Err(usage("select [TAB] (--ref eN | --selector CSS) VALUE"));
            }
            (
                BrowserOp::Select {
                    ref_,
                    selector,
                    value,
                },
                c,
            )
        }
        "press" => {
            let c = parse_common(args, &["ref", "selector"], &[], true)?;
            let key = c
                .positionals
                .first()
                .cloned()
                .ok_or_else(|| usage("press [TAB] KEY [--ref eN | --selector CSS]"))?;
            (
                BrowserOp::Press {
                    key,
                    ref_: c.value("ref").map(str::to_string),
                    selector: c.value("selector").map(str::to_string),
                },
                c,
            )
        }
        "batch" => {
            let c = parse_common(
                args,
                &["file", "final"],
                &["continue", "close-opened", "no-animate"],
                true,
            )?;
            let source = match c.value("file") {
                Some(file) => {
                    std::fs::read_to_string(file).map_err(|err| format!("--file {file}: {err}"))?
                }
                None => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                        .map_err(|err| format!("reading steps from stdin: {err}"))?;
                    text
                }
            };
            let ops: Vec<crate::api::schema::BrowserBatchStep> =
                serde_json::from_str(source.trim()).map_err(|err| {
                    format!(
                        "batch steps must be a JSON array of ops ({err}); e.g. [{{\"op\":\"fill\",\"ref\":\"e3\",\"text\":\"x\"}},{{\"op\":\"click\",\"ref\":\"e5\"}}]"
                    )
                })?;
            (
                BrowserOp::Batch {
                    ops,
                    stop_on_error: !c.flag("continue"),
                    final_: c.value("final").map(str::to_string),
                    close_opened: c.flag("close-opened"),
                    animate: !c.flag("no-animate"),
                },
                c,
            )
        }
        "use" => {
            let mut c = parse_common(args, &[], &[], true)?;
            if c.tab.is_none() {
                c.tab = c.positionals.first().cloned();
            }
            if c.tab.as_deref().is_none_or(str::is_empty) {
                return Err(usage("use TAB"));
            }
            (BrowserOp::Use, c)
        }
        "close" => {
            let c = parse_common(args, &[], &[], true)?;
            (BrowserOp::Close, c)
        }
        "focus" => {
            let c = parse_common(args, &[], &[], true)?;
            (BrowserOp::Focus, c)
        }
        other => return Err(format!("unknown browser verb {other:?}\n{USAGE}")),
    })
}

/// `--ref eN` or `--selector CSS`, at least one.
fn act_target(c: &Common, verb: &str) -> Result<(Option<String>, Option<String>), String> {
    let ref_ = c.value("ref").map(str::to_string);
    let selector = c.value("selector").map(str::to_string);
    if ref_.is_none() && selector.is_none() {
        return Err(format!(
            "usage: herdr browser {verb} [TAB] (--ref eN | --selector CSS) …"
        ));
    }
    Ok((ref_, selector))
}

fn absolute(path: &str) -> String {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        return path.to_string();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(&p).display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn run_op(verb: &str, args: &[String]) -> std::io::Result<i32> {
    let (op, common) = match build_op(verb, args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("{err}");
            return Ok(2);
        }
    };
    let out_file = common
        .value("out")
        .filter(|_| matches!(op, BrowserOp::Read { .. }))
        .map(str::to_string);
    let params = BrowserRunParams {
        caller: caller(common.pane.as_deref()),
        profile: common.profile.clone(),
        tab: common.tab.clone(),
        op,
        timeout_ms: common.timeout_ms,
    };
    let response = super::send_request(&Request {
        id: format!("cli:browser:{verb}"),
        method: Method::BrowserRun(params),
    })?;
    if common.json {
        return super::print_response(&response);
    }
    if let Some(error) = response.get("error") {
        eprintln!(
            "error {}: {}",
            error["code"].as_str().unwrap_or("error"),
            error["message"].as_str().unwrap_or("")
        );
        return Ok(1);
    }
    let result: BrowserRunResult = serde_json::from_value(response["result"]["result"].clone())
        .map_err(std::io::Error::other)?;
    print_result(&result, out_file.as_deref())
}

/// Text rendering shared by the verbs: header line, then the body.
pub(crate) fn print_result(
    result: &BrowserRunResult,
    out_file: Option<&str>,
) -> std::io::Result<i32> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", result.header)?;
    if let Some(out) = out_file {
        let content = result.data["content"].as_str().unwrap_or("");
        std::fs::write(out, content)?;
        writeln!(stdout, "[{} chars → {out}]", content.chars().count())?;
        return Ok(0);
    }
    if !result.text.is_empty() {
        writeln!(stdout, "{}", result.text)?;
    }
    Ok(0)
}

// ----- status / log / start / stop / profile -----------------------------

fn status(args: &[String]) -> std::io::Result<i32> {
    let json = args.iter().any(|a| a == "--json");
    if args.iter().any(|a| a != "--json") {
        eprintln!("usage: herdr browser status [--json]");
        return Ok(2);
    }
    let response = super::send_request(&Request {
        id: "cli:browser:status".into(),
        method: Method::BrowserStatus(EmptyParams::default()),
    })?;
    if response.get("error").is_some() || json {
        return super::print_response(&response);
    }
    let status: BrowserStatusInfo = serde_json::from_value(response["result"]["status"].clone())
        .map_err(std::io::Error::other)?;
    print!("{}", format_status(&status, crate::browser::unix_now()));
    Ok(0)
}

pub(crate) fn format_status(status: &BrowserStatusInfo, now: u64) -> String {
    use crate::browser::shape::{age, sanitize};
    let mut out = String::new();
    let get = &status.get;
    out.push_str(&format!(
        "browser: {} · default profile {} · autostart {}\n",
        if get.enabled { "enabled" } else { "disabled" },
        status.default_profile,
        if status.autostart { "on" } else { "off" }
    ));
    match (&status.executable, &status.executable_error) {
        (Some(exe), _) => out.push_str(&format!("executable: {exe}\n")),
        (None, Some(err)) => out.push_str(&format!("executable: missing ({err})\n")),
        _ => {}
    }
    out.push_str(&format!(
        "sidecar: {}{}{}\n",
        get.host.state,
        get.host
            .playwright
            .as_deref()
            .map(|v| format!(" · playwright-core {v}"))
            .unwrap_or_default(),
        get.host
            .node
            .as_deref()
            .or(status.node.as_deref())
            .map(|n| format!(" · node {n}"))
            .unwrap_or_default()
    ));
    if let Some(error) = &get.host.error {
        out.push_str(&format!("  {error}\n"));
    }
    match &status.runtime {
        Some(runtime) => out.push_str(&format!(
            "runtime: installed{} (playwright-core {}){}\n",
            runtime
                .node_version
                .as_deref()
                .map(|v| format!(" with node {v}"))
                .unwrap_or_default(),
            runtime.playwright_core.as_deref().unwrap_or("?"),
            if status.runtime_outdated {
                " · assets outdated, run `herdr browser setup`"
            } else {
                ""
            }
        )),
        None => out.push_str("runtime: not installed · run `herdr browser setup`\n"),
    }
    out.push_str(&format!("home: {}\n", status.home));
    if get.profiles.is_empty() {
        out.push_str("profiles: none running\n");
    }
    for profile in &get.profiles {
        out.push_str(&format!(
            "profile {}: {}{}{}{}{}\n",
            profile.name,
            profile.state,
            profile
                .pid
                .map(|pid| format!(" · pid {pid}"))
                .unwrap_or_default(),
            profile
                .port
                .map(|port| format!(" · port {port}"))
                .unwrap_or_default(),
            if profile.state == "running" {
                format!(
                    " · {} tabs · {} agents{}",
                    profile.tabs,
                    profile.agents,
                    if profile.dialogs > 0 {
                        format!(" · {} dialog(s) open", profile.dialogs)
                    } else {
                        String::new()
                    }
                )
            } else {
                String::new()
            },
            profile
                .detail
                .as_deref()
                .map(|d| format!(" · {d}"))
                .unwrap_or_default()
        ));
        if let Some(companion) = &profile.companion {
            out.push_str(&format!(
                "  tab groups (companion extension): {companion}\n"
            ));
        }
    }
    for tab in &get.tabs {
        let who = match &tab.last {
            Some(touch) => format!(
                "{} {} {} ago",
                sanitize(&touch.actor.label()),
                touch.op,
                age(now.saturating_sub(touch.at))
            ),
            None => format!("opened by {}", sanitize(&tab.opened_by.label())),
        };
        out.push_str(&format!(
            "  {:<9} {:<50} {}{}{}\n",
            tab.id,
            sanitize(&crate::browser::state::display_url(&tab.url))
                .chars()
                .take(50)
                .collect::<String>(),
            who,
            if tab.last_actor.is_user() && tab.last.is_some() {
                " · user since"
            } else {
                ""
            },
            if tab.dialog_open {
                " · dialog open"
            } else {
                ""
            }
        ));
    }
    for cursor in &get.recent_panes {
        out.push_str(&format!(
            "pane {}: current {} · {} ago\n",
            cursor.pane_id,
            cursor.current,
            age(now.saturating_sub(cursor.last_at))
        ));
    }
    if !status.log.is_empty() {
        out.push_str("recent:\n");
        for entry in status.log.iter().take(10) {
            out.push_str(&format!("  {}\n", format_activity(entry, now)));
        }
    }
    out
}

pub(crate) fn format_activity(entry: &BrowserActivity, now: u64) -> String {
    format!(
        "{:>5} {:<10} {:<22} {:<10} {}{}",
        crate::browser::shape::age(now.saturating_sub(entry.at)),
        entry.tab.as_deref().unwrap_or(&entry.profile),
        crate::browser::shape::sanitize(&entry.actor.label())
            .chars()
            .take(22)
            .collect::<String>(),
        entry.op,
        crate::browser::shape::sanitize(&entry.detail),
        if entry.ok {
            String::new()
        } else {
            " ✗".to_string()
        }
    )
}

fn log(args: &[String]) -> std::io::Result<i32> {
    let common = match parse_common(args, &["n"], &[], false) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("{err}\nusage: herdr browser log [-n N] [--pane ID] [--tab T] [--json]");
            return Ok(2);
        }
    };
    let limit = match common.u32_value("n") {
        Ok(limit) => limit.or_else(|| common.positionals.first().and_then(|p| p.parse().ok())),
        Err(err) => {
            eprintln!("{err}");
            return Ok(2);
        }
    };
    let response = super::send_request(&Request {
        id: "cli:browser:log".into(),
        method: Method::BrowserLog(BrowserLogParams {
            limit,
            pane_id: common.pane.clone(),
            tab: common.tab.clone(),
        }),
    })?;
    if response.get("error").is_some() || common.json {
        return super::print_response(&response);
    }
    let entries: Vec<BrowserActivity> =
        serde_json::from_value(response["result"]["entries"].clone())
            .map_err(std::io::Error::other)?;
    let now = crate::browser::unix_now();
    if entries.is_empty() {
        println!("no browser activity");
    }
    for entry in &entries {
        println!("{}", format_activity(entry, now));
    }
    Ok(0)
}

fn start_stop(args: &[String], start: bool) -> std::io::Result<i32> {
    let common = match parse_common(args, &[], &["all"], false) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("{err}");
            return Ok(2);
        }
    };
    let method = if start {
        Method::BrowserStart(BrowserProfileTarget {
            profile: common.profile.clone(),
        })
    } else {
        Method::BrowserStop(BrowserStopParams {
            profile: common.profile.clone(),
            all: common.flag("all"),
        })
    };
    let response = super::send_request(&Request {
        id: format!("cli:browser:{}", if start { "start" } else { "stop" }),
        method,
    })?;
    if response.get("error").is_some() || common.json {
        return super::print_response(&response);
    }
    println!(
        "{} requested; `herdr browser status` shows the result",
        if start { "start" } else { "stop" }
    );
    Ok(0)
}

fn profile(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: herdr browser profile list [--json] | create [--temp] NAME | delete NAME";
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let json = args.iter().any(|a| a == "--json");
            let response = super::send_request(&Request {
                id: "cli:browser:profiles".into(),
                method: Method::BrowserProfiles(EmptyParams::default()),
            })?;
            if response.get("error").is_some() || json {
                return super::print_response(&response);
            }
            for profile in response["result"]["profiles"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                println!(
                    "{:<20} {:<9}{}{}",
                    profile["name"].as_str().unwrap_or(""),
                    profile["state"].as_str().unwrap_or(""),
                    if profile["temporary"].as_bool().unwrap_or(false) {
                        " temporary"
                    } else {
                        ""
                    },
                    if profile["exists"].as_bool().unwrap_or(false) {
                        ""
                    } else {
                        " (not created yet)"
                    }
                );
            }
            Ok(0)
        }
        Some("create") => {
            let temporary = args.iter().any(|a| a == "--temp");
            let Some(name) = args[1..].iter().find(|a| !a.starts_with("--")).cloned() else {
                eprintln!("{usage}");
                return Ok(2);
            };
            let response = super::send_request(&Request {
                id: "cli:browser:profile_create".into(),
                method: Method::BrowserProfileCreate(BrowserProfileCreateParams {
                    name,
                    temporary,
                }),
            })?;
            if response.get("error").is_some() {
                return super::print_response(&response);
            }
            println!("created");
            Ok(0)
        }
        Some("delete") | Some("remove") => {
            let Some(name) = args.get(1).cloned() else {
                eprintln!("{usage}");
                return Ok(2);
            };
            let response = super::send_request(&Request {
                id: "cli:browser:profile_delete".into(),
                method: Method::BrowserProfileDelete(BrowserProfileName { name: name.clone() }),
            })?;
            if response.get("error").is_some() {
                return super::print_response(&response);
            }
            println!("profile {name} moved to the Trash");
            Ok(0)
        }
        _ => {
            eprintln!("{usage}");
            Ok(2)
        }
    }
}

// ----- setup / doctor ----------------------------------------------------

fn run_capture(program: &Path, args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .output()
        .map_err(|err| format!("{}: {err}", program.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} {} failed: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn claude_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("claude"))
        .find(|candidate| candidate.is_file())
}

/// The shell line both registrations run: `HERDR_BIN_PATH` expands inside a
/// herdr pane (so the dev instance reaches its own binary); outside herdr the
/// absolute fallback keeps the server from showing as failed.
pub(crate) fn mcp_shell_command(fallback_binary: &Path) -> String {
    format!(
        "exec \"${{HERDR_BIN_PATH:-{}}}\" browser mcp",
        fallback_binary.display()
    )
}

/// The MCP entry `setup` registers with Claude Code.
pub(crate) fn mcp_entry_json(fallback_binary: &Path) -> String {
    serde_json::json!({
        "type": "stdio",
        "command": "sh",
        "args": ["-c", mcp_shell_command(fallback_binary)],
    })
    .to_string()
}

/// The pane variables Codex must forward to the server: Codex starts MCP
/// servers with a minimal environment (HOME, PATH, …), so without this list
/// the server cannot find the pane's herdr socket or attribute the caller.
pub(crate) const CODEX_FORWARDED_ENV: [&str; 7] = [
    "HERDR_PANE_ID",
    "HERDR_BIN_PATH",
    "HERDR_SOCKET_PATH",
    "HERDR_SESSION",
    "HERDR_ENV",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
];

/// The `[mcp_servers.herdr-browser]` table `setup --codex` writes.
pub(crate) fn codex_mcp_block(fallback_binary: &Path) -> String {
    let mut table = toml::value::Table::new();
    table.insert("command".into(), toml::Value::String("sh".into()));
    table.insert(
        "args".into(),
        toml::Value::Array(vec![
            toml::Value::String("-c".into()),
            toml::Value::String(mcp_shell_command(fallback_binary)),
        ]),
    );
    table.insert(
        "env_vars".into(),
        toml::Value::Array(
            CODEX_FORWARDED_ENV
                .iter()
                .map(|v| toml::Value::String((*v).into()))
                .collect(),
        ),
    );
    let body = toml::to_string(&toml::Value::Table(table)).unwrap_or_default();
    format!("[mcp_servers.{MCP_SERVER_NAME}]\n{body}")
}

/// Put `block` (a `[mcp_servers.herdr-browser]` table) into a Codex
/// `config.toml`: an existing table of that name is replaced in place (the
/// rest of the file is untouched), else the block goes at the end.
pub(crate) fn upsert_codex_block(config: &str, block: &str) -> String {
    let header = format!("[mcp_servers.{MCP_SERVER_NAME}]");
    let is_header = |line: &str| {
        let trimmed = line.trim();
        trimmed.starts_with('[') && !trimmed.starts_with("[[") || trimmed.starts_with("[[")
    };
    let is_ours = |line: &str| {
        let trimmed = line.trim();
        trimmed == header
            || trimmed == format!("[mcp_servers.\"{MCP_SERVER_NAME}\"]")
            || trimmed.starts_with(&format!("[mcp_servers.{MCP_SERVER_NAME}."))
            || trimmed.starts_with(&format!("[mcp_servers.\"{MCP_SERVER_NAME}\"."))
    };
    let lines: Vec<&str> = config.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    let mut replaced = false;
    while i < lines.len() {
        if is_ours(lines[i]) {
            // Skip this table (and its sub-tables) up to the next header.
            i += 1;
            while i < lines.len() && (!is_header(lines[i]) || is_ours(lines[i])) {
                i += 1;
            }
            if !replaced {
                out.push(block.trim_end().to_string());
                out.push(String::new());
                replaced = true;
            }
            continue;
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    if !replaced {
        while out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(block.trim_end().to_string());
    }
    let mut text = out.join("\n");
    while text.ends_with("\n\n") {
        text.pop();
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Codex's `config.toml` (`$CODEX_HOME`, else `~/.codex`).
fn codex_config_path() -> Option<PathBuf> {
    let dir = match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".codex"),
    };
    Some(dir.join("config.toml"))
}

/// What Codex's config says about the server: `Ok(true)` registered with the
/// forwarded pane variables, `Ok(false)` absent, `Err` registered without them.
fn codex_registration(path: &Path) -> Option<Result<bool, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let entry = value.get("mcp_servers")?.get(MCP_SERVER_NAME);
    let Some(entry) = entry else {
        return Some(Ok(false));
    };
    let forwarded = entry
        .get("env_vars")
        .and_then(toml::Value::as_array)
        .is_some_and(|vars| {
            CODEX_FORWARDED_ENV
                .iter()
                .all(|v| vars.iter().any(|x| x.as_str() == Some(v)))
        });
    if forwarded {
        Some(Ok(true))
    } else {
        Some(Err(
            "registered without the forwarded pane variables (env_vars); rerun `herdr browser setup --codex`"
                .into(),
        ))
    }
}

fn register_codex(fallback: &Path) -> String {
    let Some(path) = codex_config_path() else {
        return "codex: no home directory".into();
    };
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return format!("codex: cannot read {} ({err})", path.display()),
    };
    let block = codex_mcp_block(fallback);
    let next = upsert_codex_block(&current, &block);
    if next == current {
        return format!(
            "codex: {} already registers {MCP_SERVER_NAME}",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return format!("codex: cannot create {} ({err})", parent.display());
        }
    }
    let tmp = path.with_file_name(format!(".config.toml.herdr-{}", std::process::id()));
    let written = std::fs::write(&tmp, &next).and_then(|()| std::fs::rename(&tmp, &path));
    match written {
        Ok(()) => format!(
            "codex: wrote [mcp_servers.{MCP_SERVER_NAME}] to {}:\n{}",
            path.display(),
            block
                .lines()
                .map(|l| format!("          {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            format!("codex: cannot write {} ({err})", path.display())
        }
    }
}

fn setup(args: &[String]) -> std::io::Result<i32> {
    let mut register_mcp = true;
    // Which agents to register the MCP server for: none named = both, each
    // when its tool is around.
    let mut want_claude = false;
    let mut want_codex = false;
    let mut node_override: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-mcp" => register_mcp = false,
            "--claude" => want_claude = true,
            "--codex" => want_codex = true,
            "--node" => {
                i += 1;
                node_override = args.get(i).cloned();
            }
            "help" | "--help" | "-h" => {
                println!("usage: herdr browser setup [--claude] [--codex] [--no-mcp] [--node PATH]\nInstalls the Playwright sidecar under the browser home (npm ci), records the node used, and registers the herdr-browser MCP server for Claude Code (user scope) and/or Codex (~/.codex/config.toml); with neither flag, both when found.");
                return Ok(0);
            }
            other => {
                eprintln!(
                    "unknown flag {other}\nusage: herdr browser setup [--claude] [--codex] [--no-mcp] [--node PATH]"
                );
                return Ok(2);
            }
        }
        i += 1;
    }
    let both = !want_claude && !want_codex;
    let (want_claude, want_codex) = (want_claude || both, want_codex || both);
    let config = crate::config::Config::load().config.browser;
    let home = crate::browser::browser_home();
    let host_dir = home.join(crate::integration::browser_assets::HOST_DIR);
    println!("herdr browser setup");
    println!("  home:   {}", home.display());

    // 1. assets
    let written = crate::integration::browser_assets::install(&host_dir)?;
    println!(
        "  assets: {} ({} file(s) written)",
        host_dir.display(),
        written
    );

    // 2. node
    let recorded = crate::integration::browser_assets::read_runtime(&host_dir);
    let node = crate::browser::node::discover_default(
        node_override.as_deref().or(config.node()),
        recorded.as_ref().map(|r| Path::new(&r.node)),
    );
    let Some(node) = node else {
        eprintln!("  node:   not found. Install Node.js 20+ (nvm, Homebrew) or pass --node PATH / set [browser] node.");
        return Ok(1);
    };
    let node_version = match run_capture(&node.path, &["--version"], None) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("  node:   {} does not run ({err})", node.path.display());
            return Ok(1);
        }
    };
    if !crate::browser::node::version_ok(&node_version) {
        eprintln!(
            "  node:   {} is {node_version}; 20 or newer is required",
            node.path.display()
        );
        return Ok(1);
    }
    println!(
        "  node:   {} ({node_version}, from {})",
        node.path.display(),
        node.source
    );

    // 3. npm ci
    let npm = crate::browser::node::npm_beside(&node.path).or_else(|| {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|d| d.join("npm"))
                .find(|p| p.is_file())
        })
    });
    let Some(npm) = npm else {
        eprintln!(
            "  npm:    not found beside {} or on PATH",
            node.path.display()
        );
        return Ok(1);
    };
    println!(
        "  npm:    {} · running npm ci in {}",
        npm.display(),
        host_dir.display()
    );
    let mut command = std::process::Command::new(&npm);
    command
        .args([
            "ci",
            "--omit=dev",
            "--no-audit",
            "--no-fund",
            "--ignore-scripts",
        ])
        .current_dir(&host_dir)
        .env("PATH", {
            let mut paths = vec![node
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default()];
            if let Some(path) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&path));
            }
            std::env::join_paths(paths).unwrap_or_default()
        })
        .stdin(std::process::Stdio::null());
    let status = command.status()?;
    if !status.success() {
        eprintln!(
            "  npm ci failed ({status}); fix the error above and rerun `herdr browser setup`"
        );
        return Ok(1);
    }
    let Some(playwright) = crate::integration::browser_assets::playwright_installed(&host_dir)
    else {
        eprintln!("  npm ci finished but node_modules/playwright-core is missing");
        return Ok(1);
    };
    println!("  playwright-core: {playwright}");

    // 4. runtime.json
    let runtime = crate::api::schema::BrowserRuntimeInfo {
        version: crate::integration::browser_assets::RUNTIME_VERSION,
        node: node.path.display().to_string(),
        node_version: Some(node_version),
        npm: Some(npm.display().to_string()),
        playwright_core: Some(playwright),
        assets_sha256: crate::integration::browser_assets::assets_sha256(),
        installed_at: crate::browser::unix_now(),
    };
    crate::integration::browser_assets::write_runtime(&host_dir, &runtime)?;
    println!("  runtime.json written");

    // 5. executable
    let home_env = std::env::var_os("HOME").map(PathBuf::from);
    match crate::browser::launch::resolve_executable(&config.executable, home_env.as_deref()) {
        Ok(exe) => println!("  chromium: {} ({})", exe.display(), exe.source),
        Err(err) => println!(
            "  chromium: {} — set [browser] executable before the first `browser open`",
            err.message
        ),
    }

    // 6. MCP registration
    let fallback = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("herdr"));
    let entry = mcp_entry_json(&fallback);
    if !register_mcp {
        println!("  mcp:    skipped (--no-mcp). To register by hand:\n          claude mcp add-json --scope user {MCP_SERVER_NAME} '{entry}'\n          or add this to ~/.codex/config.toml:\n{}", codex_mcp_block(&fallback).lines().map(|l| format!("          {l}")).collect::<Vec<_>>().join("\n"));
    } else if !want_claude {
        // --codex alone
    } else if let Some(claude) = claude_on_path() {
        let _ = std::process::Command::new(&claude)
            .args(["mcp", "remove", "-s", "user", MCP_SERVER_NAME])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match run_capture(&claude, &["mcp", "add-json", "--scope", "user", MCP_SERVER_NAME, &entry], None) {
            Ok(_) => println!("  mcp:    registered {MCP_SERVER_NAME} (user scope) → {entry}"),
            Err(err) => println!("  mcp:    registration failed ({err}); run by hand:\n          claude mcp add-json --scope user {MCP_SERVER_NAME} '{entry}'"),
        }
    } else {
        println!("  mcp:    `claude` not on PATH; when it is:\n          claude mcp add-json --scope user {MCP_SERVER_NAME} '{entry}'");
    }
    if register_mcp && want_codex {
        let codex_present =
            codex_on_path().is_some() || codex_config_path().is_some_and(|p| p.exists());
        if codex_present || !both {
            println!("  {}", register_codex(&fallback));
        } else {
            println!("  codex:  not found (no `codex` on PATH, no ~/.codex/config.toml); `herdr browser setup --codex` registers it anyway");
        }
    }
    println!("done. Try: herdr browser open https://example.com && herdr browser read");
    Ok(0)
}

fn codex_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("codex"))
        .find(|candidate| candidate.is_file())
}

fn install_chromium(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr browser install-chromium <Chromium.app> [--icon PNG|ICNS] [--name NAME] [--dest DIR]\nCopies the built Chromium.app to <dest>/<name>.app (default ~/Applications/herdr+ Browser.app), sets its name in Info.plist and every InfoPlist.strings, replaces the icon (the embedded herdr+ icon, or --icon), ad-hoc signs and verifies the copy, refreshes LaunchServices. The bundle id stays org.chromium.Chromium (the keychain item keeps working).";
    let mut source: Option<PathBuf> = None;
    let mut icon: Option<PathBuf> = None;
    let mut name = crate::browser::brand::DEFAULT_APP_NAME.to_string();
    let mut dest: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let value = |i: &mut usize| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match arg {
            "--icon" => icon = value(&mut i).map(PathBuf::from),
            "--name" => {
                if let Some(v) = value(&mut i) {
                    name = v;
                }
            }
            "--dest" => dest = value(&mut i).map(PathBuf::from),
            "help" | "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(0);
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag {other}\n{USAGE}");
                return Ok(2);
            }
            other => source = Some(PathBuf::from(other)),
        }
        i += 1;
    }
    let Some(source) = source else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let dest_dir = match dest {
        Some(dest) => dest,
        None => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join("Applications"),
            None => {
                eprintln!("no HOME; give --dest");
                return Ok(2);
            }
        },
    };
    let absolute = |p: PathBuf| -> PathBuf {
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir().map(|cwd| cwd.join(&p)).unwrap_or(p)
        }
    };
    let options = crate::browser::brand::InstallOptions {
        source: absolute(source),
        dest_dir: absolute(dest_dir),
        name,
        icon: icon.map(absolute),
    };
    println!("herdr browser install-chromium");
    match crate::browser::brand::install(&options) {
        Ok(report) => {
            for step in &report.steps {
                println!("  {step}");
            }
            println!(
                "installed {} — [browser] executable = \"auto\" finds it first; `herdr browser doctor` shows which",
                report.target.display()
            );
            Ok(0)
        }
        Err(err) => {
            eprintln!("error {}: {}", err.code, err.message);
            Ok(1)
        }
    }
}

fn doctor(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() && args[0] != "--json" {
        eprintln!("usage: herdr browser doctor");
        return Ok(2);
    }
    let config = crate::config::Config::load().config.browser;
    let home = crate::browser::browser_home();
    let host_dir = home.join(crate::integration::browser_assets::HOST_DIR);
    let mut problems = 0;
    let mut check = |ok: bool, line: String| {
        println!("{} {line}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            problems += 1;
        }
    };
    check(
        config.enabled,
        format!("[browser] enabled = {}", config.enabled),
    );
    let home_env = std::env::var_os("HOME").map(PathBuf::from);
    match crate::browser::launch::resolve_executable(&config.executable, home_env.as_deref()) {
        Ok(exe) => check(
            true,
            format!("chromium: {} ({})", exe.display(), exe.source),
        ),
        Err(err) => check(false, format!("chromium: {}", err.message)),
    }
    let runtime = crate::integration::browser_assets::read_runtime(&host_dir);
    check(
        runtime.is_some(),
        format!(
            "runtime.json: {}",
            if runtime.is_some() {
                "present"
            } else {
                "missing — run `herdr browser setup`"
            }
        ),
    );
    if let Some(runtime) = &runtime {
        let fresh = runtime.assets_sha256 == crate::integration::browser_assets::assets_sha256();
        check(
            fresh,
            format!(
                "sidecar assets: {}",
                if fresh {
                    "current"
                } else {
                    "outdated — run `herdr browser setup`"
                }
            ),
        );
    }
    match crate::integration::browser_assets::playwright_installed(&host_dir) {
        Some(version) => check(
            version == crate::integration::browser_assets::PLAYWRIGHT_CORE_VERSION,
            format!(
                "playwright-core: {version} (expected {})",
                crate::integration::browser_assets::PLAYWRIGHT_CORE_VERSION
            ),
        ),
        None => check(
            false,
            "playwright-core: not installed — run `herdr browser setup`".into(),
        ),
    }
    match crate::browser::node::discover_default(
        config.node(),
        runtime.as_ref().map(|r| Path::new(&r.node)),
    ) {
        Some(node) => {
            let version = run_capture(&node.path, &["--version"], None).unwrap_or_else(|err| err);
            check(
                crate::browser::node::version_ok(&version),
                format!(
                    "node: {} {} ({})",
                    node.path.display(),
                    version,
                    node.source
                ),
            );
        }
        None => check(false, "node: not found".into()),
    }
    #[cfg(target_os = "macos")]
    {
        let manager =
            run_capture(Path::new("/bin/launchctl"), &["managername"], None).unwrap_or_default();
        println!("info launch context: {manager} (Chromium is started through LaunchServices, so a Background-context server still gets a window)");
    }
    match claude_on_path() {
        Some(claude) => {
            let registered = run_capture(&claude, &["mcp", "get", MCP_SERVER_NAME], None).is_ok();
            check(
                registered,
                format!(
                    "claude mcp: {MCP_SERVER_NAME} {}",
                    if registered {
                        "registered"
                    } else {
                        "not registered — run `herdr browser setup`"
                    }
                ),
            );
        }
        None => println!("info claude: not on PATH (MCP registration skipped)"),
    }
    match codex_config_path().and_then(|path| codex_registration(&path)) {
        Some(Ok(true)) => check(
            true,
            format!("codex mcp: {MCP_SERVER_NAME} registered (pane variables forwarded)"),
        ),
        Some(Ok(false)) => {
            if codex_on_path().is_some() {
                check(
                    false,
                    format!("codex mcp: {MCP_SERVER_NAME} not registered — run `herdr browser setup --codex`"),
                );
            } else {
                println!("info codex mcp: {MCP_SERVER_NAME} not registered (no `codex` on PATH)");
            }
        }
        Some(Err(problem)) => check(false, format!("codex mcp: {MCP_SERVER_NAME} {problem}")),
        None => println!("info codex: no config.toml (registration skipped)"),
    }
    match super::send_request_unchecked(&Request {
        id: "cli:browser:doctor".into(),
        method: Method::BrowserGet(BrowserGetParams::default()),
    }) {
        Ok(response) if response.get("error").is_none() => {
            let host = response["result"]["browser"]["host"]["state"]
                .as_str()
                .unwrap_or("?");
            let profiles = response["result"]["browser"]["profiles"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            check(
                true,
                format!("server: reachable · sidecar {host} · {profiles} profile(s) known"),
            );
            for profile in response["result"]["browser"]["profiles"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let (Some(name), Some(companion)) =
                    (profile["name"].as_str(), profile["companion"].as_str())
                {
                    println!(
                        "info profile {name}: tab groups (companion extension) {companion}{}",
                        if companion.starts_with("missing") {
                            " — Chromium ignored --load-extension; the frame and cursor still work"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        Ok(response) => check(
            false,
            format!(
                "server: {}",
                response["error"]["message"].as_str().unwrap_or("error")
            ),
        ),
        Err(err) => println!("info server: not reachable ({err}); start herdr first"),
    }
    println!(
        "{}",
        if problems == 0 {
            "all good"
        } else {
            "problems found"
        }
    );
    Ok(if problems == 0 { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn codex_block_is_upserted_without_touching_the_rest() {
        let block = codex_mcp_block(Path::new("/opt/herdr"));
        assert!(block.starts_with("[mcp_servers.herdr-browser]\n"));
        assert!(block.contains("command = \"sh\""));
        let parsed: toml::Value = toml::from_str(&block).unwrap();
        let entry = &parsed["mcp_servers"]["herdr-browser"];
        assert_eq!(
            entry["args"][1].as_str(),
            Some("exec \"${HERDR_BIN_PATH:-/opt/herdr}\" browser mcp")
        );
        let vars: Vec<&str> = entry["env_vars"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(vars, CODEX_FORWARDED_ENV);
        let other = "model = \"x\"\n\n[mcp_servers.node_repl]\ncommand = \"node\"\n\n[mcp_servers.node_repl.env]\nA = \"1\"\n";
        let once = upsert_codex_block(other, &block);
        assert!(once.starts_with(other), "existing content untouched");
        assert!(once.ends_with(&format!("{}\n", block.trim_end())));
        let value: toml::Value = toml::from_str(&once).unwrap();
        assert_eq!(
            value["mcp_servers"]["herdr-browser"]["command"].as_str(),
            Some("sh")
        );
        assert_eq!(
            value["mcp_servers"]["node_repl"]["env"]["A"].as_str(),
            Some("1")
        );
        // idempotent
        assert_eq!(upsert_codex_block(&once, &block), once);
        // an old entry in the middle (with a sub-table) is replaced in place
        let stale = "a = 1\n[mcp_servers.herdr-browser]\ncommand = \"old\"\n[mcp_servers.herdr-browser.env]\nX = \"1\"\n[features]\nrmcp_client = true\n";
        let fixed = upsert_codex_block(stale, &block);
        let value: toml::Value = toml::from_str(&fixed).unwrap();
        assert_eq!(
            value["mcp_servers"]["herdr-browser"]["command"].as_str(),
            Some("sh")
        );
        assert!(value["mcp_servers"]["herdr-browser"].get("env").is_none());
        assert_eq!(value["features"]["rmcp_client"].as_bool(), Some(true));
        assert_eq!(value["a"].as_integer(), Some(1));
        assert_eq!(fixed.matches("[mcp_servers.herdr-browser]").count(), 1);
        // an empty file gets just the block
        assert_eq!(
            upsert_codex_block("", &block),
            format!("{}\n", block.trim_end())
        );
    }

    #[test]
    fn verbs_parse_into_ops_with_tab_positionals_and_globals() {
        let (op, c) = build_op(
            "read",
            &s(&[
                "t3", "--format", "text", "--offset", "200", "--json", "--pane", "w2:pD",
            ]),
        )
        .unwrap();
        assert_eq!(c.tab.as_deref(), Some("t3"));
        assert!(c.json);
        assert_eq!(c.pane.as_deref(), Some("w2:pD"));
        assert!(
            matches!(op, BrowserOp::Read { format: Some(ref f), offset: Some(200), .. } if f == "text")
        );
        let (op, c) = build_op(
            "open",
            &s(&[
                "github.com/x",
                "--focus",
                "--profile",
                "work",
                "--timeout",
                "5000",
            ]),
        )
        .unwrap();
        assert_eq!(c.profile.as_deref(), Some("work"));
        assert_eq!(c.timeout_ms, Some(5000));
        assert!(
            matches!(op, BrowserOp::Open { ref url, focus: true, .. } if url == "github.com/x")
        );
        let (op, c) = build_op("find", &s(&["main:t2", "merge", "pull", "--max=5"])).unwrap();
        assert_eq!(c.tab.as_deref(), Some("main:t2"));
        assert!(
            matches!(op, BrowserOp::Find { ref query, max: Some(5), .. } if query == "merge pull")
        );
        let (op, _) = build_op("snapshot", &s(&["--interactive"])).unwrap();
        assert!(
            matches!(op, BrowserOp::Read { format: Some(ref f), interactive: true, .. } if f == "snapshot")
        );
        let (op, _) = build_op("wait", &s(&["--text", "Done", "--timeout", "12"])).unwrap();
        assert!(
            matches!(op, BrowserOp::Wait { timeout_s: Some(12), text: Some(ref t), .. } if t == "Done")
        );
        let (op, _) = build_op("scroll", &s(&["--by", "-300"])).unwrap();
        assert!(matches!(
            op,
            BrowserOp::Scroll {
                by: Some(-300),
                to: None
            }
        ));
        let (op, _) = build_op("dialog", &s(&["accept", "my", "answer"])).unwrap();
        assert!(
            matches!(op, BrowserOp::Dialog { ref action, text: Some(ref t) } if action == "accept" && t == "my answer")
        );
        let (op, _) = build_op(
            "network",
            &s(&["--failed", "--type", "xhr", "--match", "api"]),
        )
        .unwrap();
        assert!(
            matches!(op, BrowserOp::Network { failed: true, type_: Some(ref t), match_: Some(ref m), .. } if t == "xhr" && m == "api")
        );
        let (op, c) = build_op("read", &s(&["--out", "/tmp/page.md"])).unwrap();
        assert!(matches!(op, BrowserOp::Read { all: true, .. }));
        assert_eq!(c.value("out"), Some("/tmp/page.md"));
        let (op, c) = build_op("use", &s(&["t4"])).unwrap();
        assert_eq!(op, BrowserOp::Use);
        assert_eq!(c.tab.as_deref(), Some("t4"));
        let (op, c) = build_op("use", &s(&["work:t2"])).unwrap();
        assert_eq!(op, BrowserOp::Use);
        assert_eq!(c.tab.as_deref(), Some("work:t2"));
        assert!(build_op("use", &s(&[]))
            .unwrap_err()
            .starts_with("usage: herdr browser use"));
        let (op, c) = build_op("close", &s(&["t7"])).unwrap();
        assert_eq!(op, BrowserOp::Close);
        assert_eq!(c.tab.as_deref(), Some("t7"));
        let (_, c) = build_op("tabs", &s(&["--mine"])).unwrap();
        assert!(c.flag("mine"));
        let (op, c) = build_op("click", &s(&["t3", "--ref", "e11"])).unwrap();
        assert_eq!(c.tab.as_deref(), Some("t3"));
        assert!(matches!(op, BrowserOp::Click { ref_: Some(ref r), selector: None } if r == "e11"));
        let (op, _) = build_op(
            "type",
            &s(&["--selector", "#name", "hello", "world", "--submit"]),
        )
        .unwrap();
        assert!(
            matches!(op, BrowserOp::Type { ref text, submit: true, clear: false, .. } if text == "hello world")
        );
        let (op, _) = build_op("fill", &s(&["--ref", "e14", "x"])).unwrap();
        assert!(matches!(op, BrowserOp::Fill { ref text, .. } if text == "x"));
        let (op, _) = build_op("select", &s(&["--ref", "e2", "b"])).unwrap();
        assert!(matches!(op, BrowserOp::Select { ref value, .. } if value == "b"));
        let (op, _) = build_op("press", &s(&["Enter"])).unwrap();
        assert!(matches!(op, BrowserOp::Press { ref key, ref_: None, .. } if key == "Enter"));
        let (op, _) = build_op("hover", &s(&["--selector", "#hov"])).unwrap();
        assert!(matches!(
            op,
            BrowserOp::Hover {
                selector: Some(_),
                ..
            }
        ));
        assert!(build_op("click", &s(&[]))
            .unwrap_err()
            .contains("--ref eN | --selector CSS"));
        assert!(build_op("type", &s(&["--ref", "e1"]))
            .unwrap_err()
            .contains("TEXT"));
        let dir = std::env::temp_dir().join(format!("herdr-browser-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("steps.json");
        std::fs::write(
            &file,
            r#"[{"op":"fill","ref":"e1","text":"x"},{"op":"click","ref":"e2"}]"#,
        )
        .unwrap();
        let (op, _) = build_op(
            "batch",
            &s(&[
                "--file",
                file.to_str().unwrap(),
                "--continue",
                "--final",
                "snapshot",
            ]),
        )
        .unwrap();
        assert!(
            matches!(op, BrowserOp::Batch { ref ops, stop_on_error: false, final_: Some(ref f), close_opened: false, animate: true } if ops.len() == 2 && f == "snapshot")
        );
        std::fs::write(&file, "not json").unwrap();
        assert!(build_op("batch", &s(&["--file", file.to_str().unwrap()]))
            .unwrap_err()
            .contains("JSON array"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_name_the_usage_and_unknown_flags() {
        assert!(build_op("open", &s(&[]))
            .unwrap_err()
            .starts_with("usage: herdr browser open"));
        assert!(build_op("wait", &s(&["t1"]))
            .unwrap_err()
            .contains("--text"));
        assert!(build_op("scroll", &s(&["--by", "far"]))
            .unwrap_err()
            .contains("pixels"));
        assert!(build_op("read", &s(&["--bogus"]))
            .unwrap_err()
            .contains("unknown flag --bogus"));
        assert!(build_op("teleport", &s(&[]))
            .unwrap_err()
            .contains("unknown browser verb"));
        assert!(build_op("read", &s(&["--offset", "x"]))
            .unwrap_err()
            .contains("--offset expects a number"));
        // After `--` nothing is a tab id: the text starts with the tab-like word.
        let (op, c) = build_op("type", &s(&["--ref", "e3", "--", "t5", "abc"])).unwrap();
        assert!(c.tab.is_none());
        assert!(matches!(op, BrowserOp::Type { ref text, .. } if text == "t5 abc"));
        let (_, c) = build_op("type", &s(&["t5", "--ref", "e3", "--", "t6", "x"])).unwrap();
        assert_eq!(c.tab.as_deref(), Some("t5"), "a tab before -- still counts");
        assert!(!looks_like_tab("t"));
        assert!(!looks_like_tab("tx3"));
        assert!(looks_like_tab("t12"));
        assert!(looks_like_tab("work:t1"));
        assert!(!looks_like_tab("Work:t1"));
    }

    #[test]
    fn the_mcp_entry_expands_herdr_bin_path_with_a_fallback() {
        let entry = mcp_entry_json(Path::new("/Users/me/.local/bin/herdr"));
        let value: serde_json::Value = serde_json::from_str(&entry).unwrap();
        assert_eq!(value["command"], "sh");
        assert_eq!(value["args"][0], "-c");
        assert_eq!(
            value["args"][1],
            "exec \"${HERDR_BIN_PATH:-/Users/me/.local/bin/herdr}\" browser mcp"
        );
        assert_eq!(value["type"], "stdio");
    }

    #[test]
    fn status_text_names_profiles_tabs_and_actors() {
        let mut status = BrowserStatusInfo::default();
        status.get.enabled = true;
        status.default_profile = "main".into();
        status.get.host.state = "running".into();
        status.get.host.playwright = Some("1.63.0".into());
        status
            .get
            .profiles
            .push(crate::api::schema::BrowserProfileInfo {
                name: "main".into(),
                state: "running".into(),
                pid: Some(7),
                port: Some(9),
                tabs: 1,
                agents: 1,
                ..Default::default()
            });
        status.get.tabs.push(crate::api::schema::BrowserTabInfo {
            id: "main:t1".into(),
            profile: "main".into(),
            target_id: "T".into(),
            url: "https://github.com/x".into(),
            title: "X".into(),
            selected: true,
            opened_by: crate::api::schema::BrowserActor::User,
            last: Some(crate::api::schema::BrowserTouch {
                actor: crate::api::schema::BrowserActor::Pane {
                    pane_id: "w2:pD".into(),
                    tab_id: "w2:tD".into(),
                    workspace_id: "w2".into(),
                    tab_label: "planner".into(),
                    workspace_label: None,
                    agent: Some("claude".into()),
                    session: "default".into(),
                    gone: false,
                },
                op: "read".into(),
                detail: "markdown".into(),
                at: 90,
                ok: true,
            }),
            last_actor: crate::api::schema::BrowserActor::User,
            users: vec!["w2:pD".into()],
            dialog_open: false,
            console_errors: 0,
            active: true,
            state: "open".into(),
            closed_at: None,
        });
        let text = format_status(&status, 100);
        assert!(text.contains("browser: enabled · default profile main"));
        assert!(text.contains("sidecar: running · playwright-core 1.63.0"));
        assert!(text.contains("profile main: running · pid 7 · port 9 · 1 tabs · 1 agents"));
        assert!(text.contains("main:t1   github.com/x"));
        assert!(text.contains("planner · claude read 10s ago · user since"));
        assert!(text.contains("runtime: not installed"));
    }
}
