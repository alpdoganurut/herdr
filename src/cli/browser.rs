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

  open URL [--focus] [--wait domcontentloaded|load|networkidle]     new background tab, page card (--focus selects it quietly)
  navigate [TAB] URL [--wait W]   back|forward|reload [TAB]
  read [TAB] [--format markdown|text|snapshot|html] [--selector CSS|--ref eN] [--offset N] [--max N] [--all] [--out FILE] [--interactive]
  snapshot [TAB] [--interactive] [--selector CSS] [--max N]          = read --format snapshot
  find [TAB] TEXT|/regex/ [--max 20] [--context 120]                  matches with char offsets
  links [TAB] [--filter TEXT] [--max 100]
  screenshot [TAB] [--full] [--ref eN|--selector CSS] [--format jpeg|png] [--out PATH] [--front]   (--front selects the tab quietly)
  console [TAB] [--level error|warn|all] [--since SEQ] [--max 50]
  network [TAB] [--failed] [--match SUBSTR] [--type xhr|fetch|document|…] [--since SEQ] [--max 50]
  wait [TAB] (--text S|--gone S|--selector CSS|--url GLOB|--load STATE) [--timeout 30]
  scroll [TAB] (--to top|bottom|eN | --by PX)
  eval [TAB] EXPR [--max 4000]
  dialog [TAB] accept [TEXT] | dismiss
  tabs [--mine]   use TAB   close [TAB]   focus [TAB]   (focus is the one call that raises the window: for the user)

Act (refs from browser snapshot; a password field is refused, ask the user):
  click [TAB] (--ref eN | --selector CSS)          hover [TAB] (--ref eN | --selector CSS)
  type [TAB] (--ref eN | --selector CSS) TEXT [--submit] [--clear]
  fill [TAB] (--ref eN | --selector CSS) TEXT      select [TAB] (--ref eN | --selector CSS) VALUE
  press [TAB] KEY [--ref eN | --selector CSS]      (no target: the focused element)
  TEXT/VALUE that begins with a tab-like word (t5) or a dash: put -- before it, e.g. type --ref e3 -- t5 abc
  batch [TAB] [--file steps.json] [--continue] [--final snapshot|screenshot] [--close-opened] [--no-animate]
        steps as a JSON array (stdin); a snapshot step gives the steps after it their refs,
        --close-opened closes the tabs the batch opened after the final step
  Reuse your current tab with navigate; open only for a separate tab. Close tabs you opened when the whole
  task is done, unless the user may want to look at them.

  status [--json]   log [-n 50] [--pane ID] [--tab T] [--json]   start|stop [--profile P] [--all]
  profile list | create [--temp] NAME | delete NAME
  setup [--claude] [--codex] [--no-mcp] [--node PATH]   doctor   mcp
  wrap <codex|claude> -- ARGS…   alias of `herdr agent wrap` ([agents] wrap; setup --shell hooks plain codex/claude)
  install-chromium <Chromium.app> [--icon PNG|ICNS] [--name 'herdr+ Browser'] [--dest ~/Applications]
        a branded copy (name, the herdr+ icon or --icon; bundle id and keychain item unchanged), ad-hoc signed; \"auto\" finds it first

Flags on every verb: --profile P (new = a fresh temporary profile), --tab T, --pane ID, --timeout MS, --json";

use crate::browser::setup::{self, mcp_entry_json, SetupEnv, MCP_SERVER_NAME};

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
        // permanent alias: older managed shell files call `herdr browser wrap`
        "wrap" => super::agent_wrap::run(rest),
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
    if get.setup_needed {
        out.push_str("setup: needed — an MCP registration or the shell hook is off; settings → browser → fix all, or `herdr browser setup` (`herdr browser doctor` explains)\n");
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

fn setup(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr browser setup [--claude] [--codex] [--shell [--remove]] [--no-mcp] [--node PATH]\nApplies [browser] mcp_agents: installs the Playwright sidecar under the browser home (npm ci) and registers — or removes — the herdr-browser MCP server for Claude Code (user scope) and Codex (~/.codex/config.toml).\n--claude / --codex register that agent only (whatever the config says); --no-mcp skips the registrations. The settings overlay's browser section runs the same steps (\"fix all\").\n--shell [--remove] only adds (or takes out) the shell hook: the guarded herdr+ line in ~/.zshrc whose managed file routes plain codex / claude in herdr+ panes through `herdr agent wrap` ([agents] wrap decides what it adds). Plain setup never edits ~/.zshrc; Settings → Agents offers the same fix after a confirmation.";
    let mut register_mcp = true;
    let mut want_claude = false;
    let mut want_codex = false;
    let mut shell = false;
    let mut shell_remove = false;
    let mut node_override: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-mcp" => register_mcp = false,
            "--claude" => want_claude = true,
            "--codex" => want_codex = true,
            "--shell" => shell = true,
            "--remove" => shell_remove = true,
            "--node" => {
                i += 1;
                node_override = args.get(i).cloned();
            }
            "help" | "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(0);
            }
            other => {
                eprintln!("unknown flag {other}\n{USAGE}");
                return Ok(2);
            }
        }
        i += 1;
    }
    let only_mcp = want_claude || want_codex;
    let only_shell = shell || shell_remove;
    let hook_action = setup_hook_action(shell, shell_remove);
    let config = crate::config::Config::load().config.browser;
    let mut env = SetupEnv::from_process();
    env.node_override = node_override;
    println!("herdr browser setup");
    println!("  home:   {}", env.browser_home.display());
    let mut failed = false;
    let mut report = |outcome: Result<String, String>| match outcome {
        Ok(line) => println!("  {line}"),
        Err(line) => {
            failed = true;
            println!("  {line}");
        }
    };
    let wants = |agent: &str| config.mcp_agents.iter().any(|a| a == agent);

    if !only_mcp && !only_shell {
        report(setup::install_helper(&env, &config, false));
        match crate::browser::launch::resolve_executable(&config.executable, env.home.as_deref()) {
            Ok(exe) => println!("  chromium: {} ({})", exe.display(), exe.source),
            Err(err) => println!(
                "  chromium: {} — set [browser] executable (or `herdr browser install-chromium <Chromium.app>`) before the first `browser open`",
                err.message
            ),
        }
    }
    if !only_shell && register_mcp {
        let claude_wanted = if only_mcp {
            want_claude
        } else {
            wants("claude")
        };
        if !only_mcp || want_claude {
            match (claude_wanted, env.claude_bin.is_some()) {
                (false, false) => println!("  claude: not on PATH; nothing to remove"),
                (true, false) => println!(
                    "  claude: not on PATH; when it is: claude mcp add-json --scope user {MCP_SERVER_NAME} '{}'",
                    mcp_entry_json(&env.binary)
                ),
                (wanted, true) => report(setup::register_claude(&env, wanted)),
            }
        }
        let codex_wanted = if only_mcp { want_codex } else { wants("codex") };
        if !only_mcp || want_codex {
            report(setup::register_codex(&env, codex_wanted));
            if codex_wanted {
                println!("  codex:  interactive codex hands sessions to a shared app-server daemon that spawns MCP servers with ITS environment;\n          inside herdr+ run codex through `herdr agent wrap codex` (the shell hook does that for plain `codex`)");
            }
        }
    } else if !only_shell {
        println!(
            "  mcp:    skipped (--no-mcp). To register by hand:\n          claude mcp add-json --scope user {MCP_SERVER_NAME} '{}'\n          or add this to ~/.codex/config.toml:\n{}",
            mcp_entry_json(&env.binary),
            setup::codex_mcp_block(&env.binary)
                .lines()
                .map(|l| format!("          {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    match hook_action {
        // typing --shell is the confirmation
        Some(wanted) => {
            report(setup::install_shell_hook(&env, wanted));
            if wanted {
                println!(
                    "  shell:  new panes pick it up; in an open one: source {}",
                    env.shell_file.display()
                );
            }
        }
        None if !only_mcp => {
            let hook = setup::shell_hook_check(&env);
            println!(
                "  shell:  hook {} ({}); not touched — Settings → Agents or `herdr browser setup --shell`",
                hook.state.as_str(),
                hook.detail
            );
        }
        None => {}
    }
    if !only_mcp && !only_shell && !failed {
        setup::mark_set_up(&env);
        println!("done. Try: herdr browser open https://example.com && herdr browser read");
    }
    Ok(if failed { 1 } else { 0 })
}

/// The doctor counts the hook as a problem when it is outdated (an old hook
/// breaks `codex` / `claude` inside agents' shells whatever the setting) or
/// missing while `[agents] wrap` is on.
fn hook_is_a_problem(state: setup::HookState, wrap: bool) -> bool {
    match state {
        setup::HookState::Ok => false,
        setup::HookState::Outdated => true,
        setup::HookState::Missing => wrap,
    }
}

/// What `setup` does to the shell hook: only an explicit `--shell` (add) or
/// `--shell --remove` / `--remove` (take out) touches `.zshrc`; plain setup
/// never does.
fn setup_hook_action(shell: bool, remove: bool) -> Option<bool> {
    (shell || remove).then_some(!remove)
}

/// `executable = "auto"` only looks for the default name in `~/Applications`;
/// any other name or place has to be configured by path.
pub(crate) fn install_hint(
    name: &str,
    dest_dir: &Path,
    home_apps: Option<&Path>,
    target: &Path,
) -> String {
    let same_dir = |a: &Path, b: &Path| {
        a == b
            || matches!(
                (std::fs::canonicalize(a), std::fs::canonicalize(b)),
                (Ok(x), Ok(y)) if x == y
            )
    };
    let default_place = name == crate::browser::brand::DEFAULT_APP_NAME
        && home_apps.is_some_and(|apps| same_dir(apps, dest_dir));
    if default_place {
        "[browser] executable = \"auto\" finds it first; `herdr browser doctor` shows which"
            .to_string()
    } else {
        format!(
            "\"auto\" only finds the default name in ~/Applications — set in config.toml:\n  [browser]\n  executable = {}",
            crate::coordinator::launch::toml_string(&target.display().to_string())
        )
    }
}

/// Whether a Codex app-server daemon runs (it spawns MCP servers with its own environment).
fn codex_daemon_running() -> bool {
    std::process::Command::new("ps")
        .args(["-axo", "command="])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|line| line.contains("app-server") && line.contains("--managed-daemon"))
        })
        .unwrap_or(false)
}

fn install_chromium(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr browser install-chromium <Chromium.app> [--icon PNG|ICNS] [--name NAME] [--dest DIR]\nCopies the built Chromium.app to <dest>/<name>.app (default ~/Applications/herdr+ Browser.app), sets its name in Info.plist and every InfoPlist.strings, replaces the icon (the embedded herdr+ icon, or --icon), ad-hoc signs and verifies the copy, refreshes LaunchServices. The bundle id stays org.chromium.Chromium (the keychain item keeps working).";
    if !cfg!(target_os = "macos") {
        eprintln!("herdr browser install-chromium is macOS only: it renames and re-signs a Chromium.app bundle (Info.plist, .icns, codesign, LaunchServices). On this system set [browser] executable = \"<path to chromium>\" instead.");
        return Ok(2);
    }
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
    let home_apps = std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications"));
    let dest_dir = match dest {
        Some(dest) => dest,
        None => match home_apps.clone() {
            Some(apps) => apps,
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
            println!("installed {}", report.target.display());
            println!(
                "{}",
                install_hint(
                    &options.name,
                    &options.dest_dir,
                    home_apps.as_deref(),
                    &report.target
                )
            );
            Ok(0)
        }
        Err(err) => {
            eprintln!("error {}: {}", err.code, err.message);
            Ok(1)
        }
    }
}

/// What a failing check's line ends with: how to fix it. The extension's
/// safe fix is a browser restart (`herdr browser setup` does not touch the
/// running worker); the others go through setup or the settings section.
fn doctor_hint(c: &crate::api::schema::BrowserCheckInfo) -> &'static str {
    use crate::api::schema::BrowserFixKind;
    match (c.ok, c.fix_kind, c.id.as_str()) {
        (true, _, _) => "",
        (false, BrowserFixKind::Safe, "extension") => " — `herdr browser stop` and open again",
        (false, BrowserFixKind::Safe, _) => {
            " — `herdr browser setup` (runs by itself after a herdr update)"
        }
        (false, BrowserFixKind::EditsFiles, _) => {
            " — `herdr browser setup`, or settings → browser → fix all"
        }
        (false, BrowserFixKind::None, _) => "",
    }
}

fn doctor(args: &[String]) -> std::io::Result<i32> {
    let json = args.first().is_some_and(|a| a == "--json");
    if !args.is_empty() && !json {
        eprintln!("usage: herdr browser doctor [--json]");
        return Ok(2);
    }
    let full = crate::config::Config::load().config;
    let (wrap, wrap_source) = full.agents_wrap();
    let config = full.browser;
    let env = SetupEnv::from_process();
    // The server's picture (a running profile's companion version); the CLI
    // can only ask over the socket.
    let server = super::send_request_unchecked(&Request {
        id: "cli:browser:doctor".into(),
        method: Method::BrowserGet(BrowserGetParams::default()),
    });
    let live: Option<crate::api::schema::BrowserGetInfo> = match &server {
        Ok(response) if response.get("error").is_none() => {
            serde_json::from_value(response["result"]["browser"].clone()).ok()
        }
        _ => None,
    };
    let checks = setup::checks(&config, &env, live.as_ref());
    let hook = setup::shell_hook_check(&env);
    let hook_problem = hook_is_a_problem(hook.state, wrap);
    let problems = checks.iter().filter(|c| !c.ok).count()
        + usize::from(!config.enabled)
        + usize::from(hook_problem);
    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
        return Ok(if problems == 0 { 0 } else { 1 });
    }
    println!(
        "{} [browser] enabled = {}",
        if config.enabled { "ok  " } else { "FAIL" },
        config.enabled
    );
    for c in &checks {
        println!(
            "{} {}: {}{}",
            if c.ok { "ok  " } else { "FAIL" },
            c.id,
            c.detail,
            doctor_hint(c)
        );
    }
    println!(
        "{} shell_hook: {} · {}{}",
        if hook_problem { "FAIL" } else { "ok  " },
        hook.state.as_str(),
        hook.detail,
        if hook_problem {
            " — settings → agents → fix, or `herdr browser setup --shell`"
        } else {
            ""
        }
    );
    println!(
        "info [agents] wrap = {wrap} (source: {}) · [browser] steer_agents = {} · disable_native_browser = {} (applied while wrapped) · mcp_agents = {:?}",
        wrap_source.as_str(),
        config.steer_agents,
        config.disable_native_browser,
        config.mcp_agents
    );
    match (&server, &live) {
        (_, Some(live)) => {
            println!(
                "info server: reachable · sidecar {} · {} profile(s) known · this herdr bundles companion v{}",
                live.host.state,
                live.profiles.len(),
                crate::integration::browser_assets::COMPANION_VERSION
            );
            for profile in &live.profiles {
                if let Some(companion) = profile.companion.as_deref() {
                    println!(
                        "info profile {}: tab groups (companion extension) {companion}{}",
                        profile.name,
                        if companion.starts_with("missing") {
                            " — Chromium ignored --load-extension; the frame and cursor still work"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        (Ok(response), None) => println!(
            "info server: {}",
            response["error"]["message"].as_str().unwrap_or("error")
        ),
        (Err(err), None) => println!("info server: not reachable ({err}); start herdr first"),
    }
    if codex_daemon_running() {
        println!("info codex: an app-server daemon is running (codex app-server --managed-daemon); it spawns MCP servers with its own environment, so inside herdr+ run `codex --no-daemon` — the shell hook's `codex` function does that");
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

    #[test]
    fn the_doctor_hint_names_the_fix_a_check_really_has() {
        use crate::api::schema::{BrowserCheckInfo, BrowserFixKind};
        let check = |id: &str, ok: bool, fix_kind: BrowserFixKind| BrowserCheckInfo {
            id: id.into(),
            ok,
            detail: String::new(),
            fixable: fix_kind != BrowserFixKind::None,
            fix_kind,
            waits_for_agents: 0,
        };
        assert_eq!(
            doctor_hint(&check("extension", false, BrowserFixKind::Safe)),
            " — `herdr browser stop` and open again",
            "setup does not touch the running worker"
        );
        assert_eq!(
            doctor_hint(&check("helper", false, BrowserFixKind::Safe)),
            " — `herdr browser setup` (runs by itself after a herdr update)"
        );
        assert_eq!(
            doctor_hint(&check("mcp_claude", false, BrowserFixKind::EditsFiles)),
            " — `herdr browser setup`, or settings → browser → fix all"
        );
        assert_eq!(
            doctor_hint(&check("extension", false, BrowserFixKind::None)),
            "",
            "withheld: no hint"
        );
        assert_eq!(
            doctor_hint(&check("extension", true, BrowserFixKind::Safe)),
            ""
        );
    }
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn plain_setup_never_touches_the_shell_hook() {
        assert_eq!(setup_hook_action(false, false), None, "plain setup");
        assert_eq!(setup_hook_action(true, false), Some(true));
        assert_eq!(setup_hook_action(true, true), Some(false));
        assert_eq!(setup_hook_action(false, true), Some(false));
        use crate::browser::setup::HookState;
        assert!(hook_is_a_problem(HookState::Outdated, false));
        assert!(!hook_is_a_problem(HookState::Missing, false));
        assert!(hook_is_a_problem(HookState::Missing, true));
        assert!(!hook_is_a_problem(HookState::Ok, true));
    }

    #[test]
    fn install_hint_points_at_auto_only_for_the_default_place() {
        let home = Path::new("/Users/me/Applications");
        let default_name = crate::browser::brand::DEFAULT_APP_NAME;
        let hint = install_hint(
            default_name,
            home,
            Some(home),
            &home.join("herdr+ Browser.app"),
        );
        assert!(hint.contains("\"auto\" finds it first"));
        let hint = install_hint(
            default_name,
            Path::new("/opt/apps"),
            Some(home),
            Path::new("/opt/apps/herdr+ Browser.app"),
        );
        assert!(hint.contains("executable = \"/opt/apps/herdr+ Browser.app\""));
        assert!(!hint.contains("finds it first"));
        let hint = install_hint("Other", home, Some(home), &home.join("Other.app"));
        assert!(hint.contains("executable = \"/Users/me/Applications/Other.app\""));
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
                    shell_pid: None,
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
