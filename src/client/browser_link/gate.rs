//! A's gate: what an `op` of the remote server (B) may do to the browser on
//! this machine. Pure: a frame, A's policy and a clock go in, a [`Verdict`]
//! comes out. The rules are data of [`link_proto`](crate::browser::link_proto)
//! (the op table, the press table, [`RefuseRule`]); the request of a
//! [`GatedOp`] is built from those rules, so what they do not name never
//! reaches the sidecar.

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::browser::host::HostRequest;
use crate::browser::hub::COMPANION_PROFILE;
use crate::browser::link_proto::{
    code, is_activity_color, is_file_token, op_rule, press_allowed, text, ArgKind, Ensure,
    LinkActivity, LinkError, LinkOp, OpGate, OpRule, Refusal, RefuseRule, ACTIVITY_MAX_CHARS,
    ACTIVITY_MAX_MS, EVAL_MAX_BYTES, PRESS_MODIFIERS, RELEASE_MAX_KEYS, STALE_ECHO_MS,
};
use crate::config::{BrowserConfig, MAX_OP_TIMEOUT_MS};

/// A's own `[browser]` keys an op is under, whatever B's say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub allow_eval: bool,
    /// `type_into_password_fields`: typing into them and no guard for `eval`.
    pub allow_password: bool,
}

impl From<&BrowserConfig> for Policy {
    fn from(config: &BrowserConfig) -> Self {
        Self {
            allow_eval: config.allow_eval,
            allow_password: config.type_into_password_fields,
        }
    }
}

/// What A does with one `op`.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The sidecar runs it.
    Run(GatedOp),
    /// The helper answers it itself.
    Helper(HelperOp),
    /// The answer's `error`; nothing runs.
    Refuse(LinkError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperOp {
    Hello,
    Ping,
}

/// An op that passed; only the gate makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct GatedOp {
    id: u64,
    rule: &'static OpRule,
    target: Option<String>,
    args: Map<String, Value>,
    deadline_ms: u64,
    activity: Option<LinkActivity>,
}

impl GatedOp {
    /// The link's id, for the `re`.
    #[cfg(test)]
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn op(&self) -> &'static str {
        self.rule.op
    }

    pub fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }

    /// The sidecar's request; `id` is the helper's own number for it.
    pub fn request(&self, id: u64) -> HostRequest<'_> {
        HostRequest {
            id,
            op: self.rule.op,
            profile: Some(COMPANION_PROFILE),
            target: self.target.as_deref(),
            args: match self.args.is_empty() {
                true => Value::Null,
                false => Value::Object(self.args.clone()),
            },
            deadline_ms: self.deadline_ms,
            activity: self
                .activity
                .as_ref()
                .and_then(|activity| serde_json::to_value(activity).ok()),
        }
    }
}

/// The gate of one link: a new link (epoch) gets a new one, its pings count
/// from 1 again. Every `now` is the platform clock that counts sleep.
#[derive(Debug)]
pub struct Gate {
    label: String,
    last_ping: u64,
    /// The pings (number, time) an op may still echo, oldest first.
    pings: VecDeque<(u64, Duration)>,
}

impl Gate {
    /// `label` is A's own name; `opened` is the time before the
    /// `browser.link_open` request was written: nothing B sends on this link
    /// is older, so it is the time of "no ping seen" (echo 0).
    pub fn new(label: impl Into<String>, opened: Duration) -> Self {
        Self {
            label: label.into(),
            last_ping: 0,
            pings: VecDeque::from([(0, opened)]),
        }
    }

    /// The next ping's number, stamped; called before the ping is queued.
    pub fn ping(&mut self, now: Duration) -> u64 {
        self.last_ping += 1;
        self.pings.push_back((self.last_ping, now));
        while self
            .pings
            .front()
            .is_some_and(|(_, sent)| !is_fresh(*sent, now))
        {
            self.pings.pop_front();
        }
        self.last_ping
    }

    /// Called right before the op would go to the sidecar: `now` read earlier
    /// would not see a sleep in between.
    pub fn op(&self, op: LinkOp, policy: Policy, now: Duration) -> Verdict {
        if self.is_stale(op.echo, now) {
            return Verdict::Refuse(LinkError::new(
                code::HOST_LINK_LOST,
                text::link_lost(&self.label, &clean(&op.op)),
            ));
        }
        match check(op, policy) {
            Ok(Passed::Run(op)) => Verdict::Run(op),
            Ok(Passed::Helper(op)) => Verdict::Helper(op),
            Err(refusal) => Verdict::Refuse(refusal.error(&self.label)),
        }
    }

    /// Whether an `ensure` may start the browser, else its `ensure_re`'s
    /// error.
    pub fn ensure(&self, ensure: &Ensure, now: Duration) -> Result<(), LinkError> {
        if self.is_stale(ensure.echo, now) {
            // an ensure is no op that may have happened
            return Err(LinkError::new(
                code::HOST_LINK_LOST,
                text::reconnecting(&self.label),
            ));
        }
        Ok(())
    }

    /// Also a ping that was never sent.
    fn is_stale(&self, echo: u64, now: Duration) -> bool {
        !self
            .pings
            .iter()
            .any(|(n, sent)| *n == echo && is_fresh(*sent, now))
    }
}

/// False for a clock that ran backwards too.
fn is_fresh(sent: Duration, now: Duration) -> bool {
    now.checked_sub(sent)
        .is_some_and(|age| age <= Duration::from_millis(STALE_ECHO_MS))
}

#[derive(Debug)]
enum Passed {
    Run(GatedOp),
    Helper(HelperOp),
}

fn check(op: LinkOp, policy: Policy) -> Result<Passed, Refusal> {
    let Some(rule) = op_rule(&op.op) else {
        return Err(refusal(RefuseRule::UnknownOp, [op.op.as_str()]));
    };
    match rule.gate {
        OpGate::Allowed => {}
        OpGate::Refused => return Err(refusal(RefuseRule::LocalOp, [rule.op])),
        OpGate::Helper => {
            return match rule.op {
                "hello" => Ok(Passed::Helper(HelperOp::Hello)),
                "ping" => Ok(Passed::Helper(HelperOp::Ping)),
                op => Err(refusal(RefuseRule::LocalOp, [op])),
            }
        }
    }
    if rule.op == "eval" && !policy.allow_eval {
        return Err(refusal(RefuseRule::EvalOff, [rule.op]));
    }
    let target = match rule.target {
        true => Some(
            op.target
                .ok_or_else(|| refusal(RefuseRule::Argument, [rule.op, " target"]))?,
        ),
        false => None,
    };
    let sent = match op.args {
        Value::Null => Map::new(),
        Value::Object(sent) => sent,
        _ => return Err(refusal(RefuseRule::Argument, [rule.op, " args"])),
    };
    Ok(Passed::Run(GatedOp {
        id: op.id,
        rule,
        target,
        args: check_args(rule, sent, policy)?,
        // B's deadline, never past what a local op may take
        deadline_ms: op.deadline_ms.min(MAX_OP_TIMEOUT_MS),
        activity: op.activity.and_then(check_activity),
    }))
}

/// The arguments that go on: what the op's rules name, nothing else.
fn check_args(
    rule: &OpRule,
    mut sent: Map<String, Value>,
    policy: Policy,
) -> Result<Map<String, Value>, Refusal> {
    let is_press = sent.get("kind").and_then(Value::as_str) == Some("press");
    let mut args = Map::new();
    for arg in rule.args {
        if arg.kind == ArgKind::Own {
            if let Some(value) = own_value(arg.name, policy) {
                args.insert(arg.name.into(), value);
            }
            continue;
        }
        let value = match sent.remove(arg.name) {
            None | Some(Value::Null) if arg.required || (arg.kind == ArgKind::Key && is_press) => {
                return Err(refusal(RefuseRule::Argument, [rule.op, " ", arg.name]))
            }
            None | Some(Value::Null) => continue,
            Some(value) => value,
        };
        let value = match (arg.kind, value) {
            (ArgKind::Key, Value::String(key)) => match press_spelling(&key) {
                Some(spelling) => spelling.into(),
                None => return Err(refusal(RefuseRule::Key, ["the key ", key.as_str()])),
            },
            (kind, value) => match check_value(kind, &value) {
                Ok(()) => value,
                Err(rule_broken) => {
                    return Err(refusal(rule_broken, [rule.op, " ", arg.name]));
                }
            },
        };
        args.insert(arg.name.into(), value);
    }
    Ok(args)
}

/// A's value of an [`ArgKind::Own`]; none for a path: A writes no file.
fn own_value(name: &str, policy: Policy) -> Option<Value> {
    match name {
        "allow_password" => Some(policy.allow_password.into()),
        "guard_passwords" => Some((!policy.allow_password).into()),
        "user" => Some(false.into()),
        _ => None,
    }
}

/// A value that goes on as it came.
fn check_value(kind: ArgKind, value: &Value) -> Result<(), RefuseRule> {
    let fits = match (kind, value) {
        (ArgKind::Text, value) => value.is_string(),
        (ArgKind::Bool, value) => value.is_boolean(),
        (ArgKind::Count, value) => value.is_u64(),
        (ArgKind::Integer, value) => value.is_i64() || value.is_u64(),
        (ArgKind::WebUrl, Value::String(url)) if !is_web_url(url) => return Err(RefuseRule::Url),
        (ArgKind::WebUrl, value) => value.is_string(),
        (ArgKind::EvalText, Value::String(expr)) if expr.len() > EVAL_MAX_BYTES => {
            return Err(RefuseRule::TooLarge)
        }
        (ArgKind::EvalText, value) => value.is_string(),
        (ArgKind::TextList, Value::Array(list)) if list.len() > RELEASE_MAX_KEYS => {
            return Err(RefuseRule::TooLarge)
        }
        (ArgKind::TextList, Value::Array(list)) => list.iter().all(Value::is_string),
        (ArgKind::FileToken, Value::String(token)) => is_file_token(token),
        (ArgKind::RelPath, Value::String(path)) => is_rel_path(path),
        (ArgKind::OneOf(values), Value::String(value)) => values.contains(&value.as_str()),
        // the rest is no text or list; the first two never go on as they came
        (
            ArgKind::Key
            | ArgKind::Own
            | ArgKind::TextList
            | ArgKind::FileToken
            | ArgKind::RelPath
            | ArgKind::OneOf(_),
            _,
        ) => false,
    };
    match fits {
        true => Ok(()),
        false => Err(RefuseRule::Argument),
    }
}

fn is_web_url(url: &str) -> bool {
    let has_scheme = |scheme: &str| {
        url.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    };
    url == "about:blank"
        || ((has_scheme("http://") || has_scheme("https://")) && !url.contains(char::is_control))
}

/// As the sidecar's `isRemoteFile`: a URL parser strips raw tabs and newlines
/// (`.\t.` is `..` to it), so no control character either.
fn is_rel_path(path: &str) -> bool {
    !path.contains(|c: char| c.is_ascii_control() || matches!(c, '\\' | '?' | '#'))
        && path.split('/').all(|segment| {
            let plain = segment.to_ascii_lowercase().replace("%2e", ".");
            !matches!(plain.as_str(), "" | "." | "..")
                && !["%2f", "%5c", "%00"].iter().any(|bad| plain.contains(bad))
        })
}

/// The table's spelling of a key: split as Playwright's `Keyboard.press` does
/// (a `+` at the start of a part is the key itself), every part but the last a
/// modifier of [`PRESS_MODIFIERS`], each once, put in that order.
fn press_spelling(key: &str) -> Option<String> {
    let mut held = [false; PRESS_MODIFIERS.len()];
    let mut key = key;
    loop {
        let first = key.chars().next().map_or(0, char::len_utf8);
        let Some(plus) = key[first..].find('+').map(|at| first + at) else {
            break;
        };
        let part = &key[..plus];
        let modifier = PRESS_MODIFIERS.iter().position(|name| *name == part)?;
        if std::mem::replace(&mut held[modifier], true) {
            return None;
        }
        key = &key[plus + 1..];
    }
    let mut spelling = String::new();
    for (modifier, _) in PRESS_MODIFIERS.iter().zip(held).filter(|(_, held)| *held) {
        spelling.push_str(modifier);
        spelling.push('+');
    }
    spelling.push_str(key);
    press_allowed(&spelling).then_some(spelling)
}

/// The activity as it goes on; none drops it and the op runs without.
fn check_activity(activity: Value) -> Option<LinkActivity> {
    // serde reads a struct from an array too
    if !activity.is_object() || activity.get("group").is_some_and(Value::is_array) {
        return None;
    }
    let mut activity: LinkActivity = serde_json::from_value(activity).ok()?;
    if !is_activity_color(&activity.color) {
        return None;
    }
    activity.linger_ms = activity.linger_ms.min(ACTIVITY_MAX_MS);
    if let Some(group) = &mut activity.group {
        for text in [&mut group.key, &mut group.title, &mut group.color] {
            *text = clean(text);
        }
        group.collapse_ms = group.collapse_ms.min(ACTIVITY_MAX_MS);
    }
    Some(activity)
}

/// A text of B for A's user: no control character, at most
/// [`ACTIVITY_MAX_CHARS`].
fn clean(text: &str) -> String {
    clean_parts([text])
}

fn clean_parts<const N: usize>(parts: [&str; N]) -> String {
    parts
        .iter()
        .flat_map(|part| part.chars())
        .filter(|c| !c.is_control())
        .take(ACTIVITY_MAX_CHARS)
        .collect()
}

fn refusal<const N: usize>(rule: RefuseRule, what: [&str; N]) -> Refusal {
    Refusal {
        rule,
        what: clean_parts(what),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use regex::Regex;
    use serde_json::json;

    use super::*;
    use crate::browser::link_proto::{ServerFrame, MAX_LINE_BYTES, OPS};

    const OPS_FIXTURE: &str = include_str!("../../../tests/fixtures/browser-link-v1/ops.json");
    const PRESS: &str = include_str!("../../../tests/fixtures/browser-link-v1/press.json");
    const TOKEN: &str = "k3v9x0q2m7w1z8r4t6y5u0ab";
    const DEFAULT: Policy = Policy {
        allow_eval: true,
        allow_password: false,
    };

    fn fixture(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn texts(value: &Value) -> Vec<&str> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    }

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn decode(line: &str) -> LinkOp {
        match ServerFrame::decode(line.as_bytes()) {
            Ok(ServerFrame::Op(op)) => op,
            other => panic!("{line}: {other:?}"),
        }
    }

    fn link_op(op: &str, target: Option<&str>, args: Value) -> LinkOp {
        LinkOp {
            id: 7,
            echo: 0,
            op: op.into(),
            target: target.map(str::to_string),
            args,
            deadline_ms: 30_000,
            activity: None,
        }
    }

    /// A page op's op on a page.
    fn page_op(op: &str, args: Value) -> LinkOp {
        link_op(op, Some("9A1F"), args)
    }

    /// `expect` of the fixture: what goes on, the helper, or the refusal.
    fn outcome(op: LinkOp, policy: Policy) -> Value {
        let gate = Gate::new("alp-mac", Duration::ZERO);
        let verdict = gate.op(
            LinkOp {
                echo: 0,
                ..op.clone()
            },
            policy,
            ms(1),
        );
        let checked = match check(op.clone(), policy) {
            Err(refusal) => {
                assert_eq!(verdict, Verdict::Refuse(refusal.error("alp-mac")));
                return json!({ "refuse": refusal });
            }
            Ok(Passed::Helper(helper)) => {
                assert_eq!(verdict, Verdict::Helper(helper));
                return json!({ "helper": true });
            }
            Ok(Passed::Run(checked)) => checked,
        };
        assert_eq!(verdict, Verdict::Run(checked.clone()));
        assert_eq!(
            (checked.id(), checked.op(), checked.deadline_ms()),
            (op.id, op.op.as_str(), op.deadline_ms)
        );
        // what the sidecar reads: its own number, A's profile, B's deadline
        let mut request = serde_json::to_value(checked.request(900)).unwrap();
        let request_fields = request.as_object_mut().unwrap();
        assert_eq!(request_fields.remove("id"), Some(json!(900)));
        assert_eq!(request_fields.remove("profile"), Some(json!("companion")));
        assert_eq!(
            request_fields.remove("deadline_ms"),
            Some(json!(op.deadline_ms))
        );
        json!({ "forward": request })
    }

    fn rule_of(op: LinkOp, policy: Policy) -> Option<RefuseRule> {
        check(op, policy).err().map(|refusal| refusal.rule)
    }

    #[test]
    fn bs_deadline_is_bounded_and_never_raised() {
        let gated = |deadline_ms| {
            let op = LinkOp {
                deadline_ms,
                ..page_op("read", json!({ "format": "markdown" }))
            };
            match check(op, DEFAULT) {
                Ok(Passed::Run(checked)) => checked.deadline_ms(),
                _ => panic!("refused"),
            }
        };
        assert_eq!(gated(u64::MAX), MAX_OP_TIMEOUT_MS);
        assert_eq!(gated(MAX_OP_TIMEOUT_MS + 1), MAX_OP_TIMEOUT_MS);
        assert_eq!(gated(MAX_OP_TIMEOUT_MS), MAX_OP_TIMEOUT_MS);
        assert_eq!(gated(30_000), 30_000);
        assert_eq!(gated(1), 1);
    }

    #[test]
    fn every_case_of_the_fixture() {
        let fixture = fixture(OPS_FIXTURE);
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 110);
        for case in cases {
            let config = BrowserConfig {
                allow_eval: case["config"]["allow_eval"].as_bool().unwrap_or(true),
                type_into_password_fields: case["config"]["type_into_password_fields"]
                    .as_bool()
                    .unwrap_or(false),
                ..BrowserConfig::default()
            };
            let op = decode(&case["frame"].to_string());
            assert_eq!(
                outcome(op, Policy::from(&config)),
                case["expect"],
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn the_policy_is_the_configs() {
        assert_eq!(Policy::from(&BrowserConfig::default()), DEFAULT);
        // both ways round: the keys are not swapped
        let config = BrowserConfig {
            allow_eval: false,
            type_into_password_fields: true,
            ..BrowserConfig::default()
        };
        assert_eq!(
            Policy::from(&config),
            Policy {
                allow_eval: false,
                allow_password: true
            }
        );
    }

    #[test]
    fn the_gate_knows_every_own_argument_and_helper_op() {
        let own: BTreeSet<&str> = OPS
            .iter()
            .flat_map(|rule| rule.args)
            .filter(|arg| arg.kind == ArgKind::Own)
            .map(|arg| arg.name)
            .collect();
        let known = [
            "allow_password",
            "guard_passwords",
            "inline_path",
            "path",
            "user",
        ];
        assert_eq!(own, BTreeSet::from(known));
        for rule in OPS.iter().filter(|rule| rule.gate == OpGate::Helper) {
            assert_eq!(
                outcome(link_op(rule.op, None, Value::Null), DEFAULT),
                json!({"helper": true})
            );
        }
    }

    // -----------------------------------------------------------------------
    // Keys

    fn press(key: &str) -> Value {
        outcome(
            page_op("act", json!({"kind": "press", "key": key})),
            DEFAULT,
        )
    }

    fn pressed(spelling: &str) -> Value {
        json!({"forward": {"op": "act", "target": "9A1F",
            "args": {"kind": "press", "key": spelling, "allow_password": false}}})
    }

    #[test]
    fn every_key_of_the_fixture() {
        let fixture = fixture(PRESS);
        let allowed = texts(&fixture["allowed"]);
        assert_eq!(allowed.len(), 401);
        for key in allowed {
            assert_eq!(press(key), pressed(key));
        }
        let reordered = fixture["reordered"].as_array().unwrap();
        assert_eq!(reordered.len(), 6);
        for entry in reordered {
            let key = entry["key"].as_str().unwrap();
            assert_eq!(press(key), pressed(entry["presses"].as_str().unwrap()));
        }
        let mut refused = texts(&fixture["refused"]);
        assert_eq!(refused.len(), 18);
        refused.extend(texts(&fixture["refused_more"]));
        assert_eq!(refused.len(), 18 + 43);
        for key in refused {
            assert_eq!(
                press(key),
                json!({"refuse": {"rule": "key", "what": format!("the key {key}")}})
            );
        }
    }

    /// Every input that may be pressed and its spelling, from the fixture
    /// alone: the modifiers of an allowed spelling in any order.
    fn pressable() -> BTreeMap<String, String> {
        fn orders(rest: &[&str]) -> Vec<String> {
            if rest.is_empty() {
                return vec![String::new()];
            }
            (0..rest.len())
                .flat_map(|first| {
                    let mut others = rest.to_vec();
                    let first = others.remove(first);
                    orders(&others)
                        .into_iter()
                        .map(move |tail| format!("{first}+{tail}"))
                })
                .collect()
        }
        let mut all = BTreeMap::new();
        for spelling in texts(&fixture(PRESS)["allowed"]) {
            let mut key = spelling;
            let mut modifiers = Vec::new();
            while let Some((modifier, rest)) = key
                .split_once('+')
                .filter(|(modifier, _)| PRESS_MODIFIERS.contains(modifier))
            {
                modifiers.push(modifier);
                key = rest;
            }
            for order in orders(&modifiers) {
                let before = all.insert(format!("{order}{key}"), spelling.to_string());
                assert_eq!(before, None, "{order}{key}");
            }
        }
        all
    }

    #[test]
    fn exactly_the_tables_keys_in_any_order_of_their_modifiers() {
        let pressable = pressable();
        assert!(pressable.len() > 401);
        let fixture = fixture(PRESS);
        let mut keys: BTreeSet<String> = ["", "+", "++", "Nope", "é", "Cmd", "Copy", "\u{1f600}"]
            .map(String::from)
            .into();
        for list in ["allowed", "refused", "refused_more"] {
            for key in texts(&fixture[list]) {
                let last = key.rfind('+').map_or(0, |at| at + 1);
                keys.insert(key[last..].into());
                keys.insert(key.into());
            }
        }
        let mut prefixes = vec![String::new()];
        let parts = PRESS_MODIFIERS
            .into_iter()
            .chain(["MetaLeft", "Cmd", "shift", "a", "", "+", " Shift"]);
        for part in parts.clone() {
            prefixes.push(format!("{part}+"));
            for second in parts.clone() {
                prefixes.push(format!("{part}+{second}+"));
                for third in PRESS_MODIFIERS {
                    prefixes.push(format!("{part}+{second}+{third}+"));
                }
            }
        }
        let mut allowed = BTreeSet::new();
        let mut tried = 0;
        for prefix in &prefixes {
            for key in &keys {
                let input = format!("{prefix}{key}");
                let spelling = press_spelling(&input);
                assert_eq!(spelling.as_ref(), pressable.get(&input), "{input:?}");
                allowed.extend(spelling);
                tried += 1;
            }
        }
        assert!(tried > 100_000, "{tried}");
        assert_eq!(allowed.len(), 401, "every spelling was reached");
    }

    #[test]
    fn a_key_is_cut_and_stripped_in_its_refusal() {
        let long = "x".repeat(MAX_LINE_BYTES / 2);
        assert_eq!(
            press(&long),
            json!({"refuse": {"rule": "key", "what": format!("the key {}", "x".repeat(72))}})
        );
        assert_eq!(
            press("Enter\n\u{1b}[0m"),
            json!({"refuse": {"rule": "key", "what": "the key Enter[0m"}})
        );
        assert_eq!(press(&"+".repeat(1 << 20))["refuse"]["rule"], "key");
        assert_eq!(press(&"Shift+".repeat(1 << 18))["refuse"]["rule"], "key");
        assert_eq!(press("Enter\0")["refuse"]["rule"], "key");
        assert_eq!(press("Meta\u{200b}+v")["refuse"]["rule"], "key");
        assert_eq!(press("a ")["refuse"]["rule"], "key");
    }

    // -----------------------------------------------------------------------
    // Sizes the fixture does not hold

    #[test]
    fn an_eval_is_at_most_256_kib() {
        let eval = |expr: String| outcome(page_op("eval", json!({ "expr": expr })), DEFAULT);
        let full = "x".repeat(EVAL_MAX_BYTES);
        assert_eq!(EVAL_MAX_BYTES, 256 * 1024);
        assert_eq!(
            eval(full.clone()),
            json!({"forward": {"op": "eval", "target": "9A1F",
                "args": {"expr": full, "guard_passwords": true}}})
        );
        let too_large = json!({"refuse": {"rule": "too_large", "what": "eval expr"}});
        assert_eq!(eval(format!("{full}x")), too_large);
        // bytes, not characters
        let wide = "é".repeat(EVAL_MAX_BYTES / 2);
        assert!(eval(wide.clone())["forward"].is_object());
        assert_eq!(eval(format!("{wide}é")), too_large);
        // an eval that may not run says that, whatever its size
        let off = Policy {
            allow_eval: false,
            ..DEFAULT
        };
        assert_eq!(
            rule_of(page_op("eval", json!({"expr": format!("{full}x")})), off),
            Some(RefuseRule::EvalOff)
        );
        // and nothing else asks for allow_eval
        for rule in OPS.iter().filter(|rule| rule.op != "eval") {
            assert_ne!(
                rule_of(page_op(rule.op, Value::Null), off),
                Some(RefuseRule::EvalOff)
            );
        }
    }

    #[test]
    fn other_texts_have_the_lines_limit_only() {
        let text = "x".repeat(1 << 20);
        let got = outcome(
            page_op("act", json!({"kind": "fill", "ref": "e4", "text": text})),
            DEFAULT,
        );
        assert_eq!(got["forward"]["args"]["text"], text);
    }

    // -----------------------------------------------------------------------
    // The stale echo

    fn tabs(echo: u64) -> LinkOp {
        LinkOp {
            echo,
            ..link_op("tabs", None, Value::Null)
        }
    }

    fn runs(gate: &Gate, echo: u64, now: u64) -> bool {
        let ensure = Ensure {
            id: 1,
            echo,
            reason: "open".into(),
        };
        let verdict = gate.op(tabs(echo), DEFAULT, ms(now));
        let started = gate.ensure(&ensure, ms(now));
        assert_eq!(started.is_ok(), matches!(verdict, Verdict::Run(_)));
        if let Err(err) = &started {
            assert_eq!(err.code, "host_link_lost");
            assert_eq!(
                err.message,
                "the connection to the browser on alp-mac dropped and is reconnecting; nothing was sent; call again in a few seconds"
            );
            assert_eq!(
                verdict,
                Verdict::Refuse(LinkError::new(
                    "host_link_lost",
                    "the connection to alp-mac dropped while tabs ran; it changed nothing, call it again"
                ))
            );
        }
        started.is_ok()
    }

    #[test]
    fn an_op_of_an_old_ping_does_not_run() {
        let mut gate = Gate::new("alp-mac", ms(1_000));
        assert_eq!(gate.ping(ms(2_000)), 1);
        assert_eq!(gate.ping(ms(5_000)), 2);
        for (echo, sent) in [(0, 1_000), (1, 2_000), (2, 5_000)] {
            assert!(runs(&gate, echo, sent.max(5_000)));
            assert!(runs(&gate, echo, sent + STALE_ECHO_MS));
            assert!(!runs(&gate, echo, sent + STALE_ECHO_MS + 1));
            // the Mac slept
            assert!(!runs(&gate, echo, sent + 8 * 3_600_000));
        }
        // a fraction of a millisecond is older too
        let late = ms(5_000 + STALE_ECHO_MS) + Duration::from_nanos(1);
        assert!(matches!(
            gate.op(tabs(2), DEFAULT, late),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn an_echo_of_no_ping_does_not_run() {
        let mut gate = Gate::new("alp-mac", ms(0));
        gate.ping(ms(10));
        for echo in [2, 3, u64::MAX] {
            assert!(!runs(&gate, echo, 20), "{echo}");
        }
        assert!(runs(&gate, 1, 20));
        // a new link knows none of the old one's
        assert!(!runs(&Gate::new("alp-mac", ms(15)), 1, 20));
        // a clock that ran backwards proves nothing
        assert!(!runs(&gate, 1, 9));
    }

    #[test]
    fn the_pings_are_not_kept_longer_than_they_count() {
        let mut gate = Gate::new("alp-mac", ms(0));
        for n in 1..=10_000u64 {
            assert_eq!(gate.ping(ms(n * 3_000)), n);
        }
        // 15 s of one each 3 s
        assert_eq!(gate.pings.len(), 6);
        assert!(runs(&gate, 9_995, 30_000_000));
        assert!(!runs(&gate, 9_994, 30_000_000));
        assert!(!runs(&gate, 0, 30_000_000));
        // after a sleep the next ping clears them all but itself
        gate.ping(ms(40_000_000));
        assert_eq!(gate.pings.len(), 1);
        assert!(runs(&gate, 10_001, 40_000_000));
    }

    #[test]
    fn a_stale_op_is_only_that() {
        let gate = Gate::new("alp-mac", ms(0));
        let stale = |op: &str| match gate.op(page_op(op, json!({"x": 1})), DEFAULT, ms(15_001)) {
            Verdict::Refuse(err) => err,
            other => panic!("{op}: {other:?}"),
        };
        for rule in OPS {
            let err = stale(rule.op);
            assert_eq!(err.code, "host_link_lost");
            assert_eq!(err.message, text::link_lost("alp-mac", rule.op));
            assert_eq!(err.target, None);
        }
        assert_eq!(
            stale("open").message,
            "the connection to alp-mac dropped while open ran: it may or may not have happened. Check with browser_read or browser_snapshot before repeating it."
        );
        let unknown = stale(&format!("a\u{1b}{}", "b".repeat(1 << 20)));
        assert!(unknown.message.len() < 400 && !unknown.message.contains('\u{1b}'));
    }

    // -----------------------------------------------------------------------
    // The exact texts

    #[test]
    fn the_refusal_texts() {
        let gate = Gate::new("alp-mac", ms(0));
        let off = Policy {
            allow_eval: false,
            ..DEFAULT
        };
        for (op, policy, message) in [
            (
                link_op("set_input_files", None, Value::Null),
                DEFAULT,
                "alp-mac refuses set_input_files for remote machines (unknown operation)",
            ),
            (
                link_op("show", None, json!({"file": "/etc/hosts"})),
                DEFAULT,
                "alp-mac refuses show for remote machines (only herdr+ on that machine may do it)",
            ),
            (
                link_op("read", None, Value::Null),
                DEFAULT,
                "alp-mac refuses read target for remote machines (missing, or not what it must be)",
            ),
            (
                link_op("release", None, json!({"keys": vec!["w1:p1"; 65]})),
                DEFAULT,
                "alp-mac refuses release keys for remote machines (too large)",
            ),
            (
                link_op("open", None, json!({"url": "file:///etc/hosts"})),
                DEFAULT,
                "alp-mac refuses open url for remote machines (only http, https and about:blank pages)",
            ),
            (
                page_op("act", json!({"kind": "press", "key": "Meta+v"})),
                DEFAULT,
                "alp-mac refuses the key Meta+v for remote machines (not one of the allowed keys)",
            ),
            (
                page_op("eval", json!({"expr": "1"})),
                off,
                "alp-mac refuses eval for remote machines ([browser] allow_eval = false there)",
            ),
        ] {
            assert_eq!(
                gate.op(op, policy, ms(1)),
                Verdict::Refuse(LinkError::new("host_refused", message))
            );
        }
    }

    // -----------------------------------------------------------------------
    // URLs, paths, activity

    #[test]
    fn only_http_https_and_about_blank() {
        for url in [
            "http://a",
            "HtTpS://a/",
            "https://user:pass@a:8443/é?q#f",
            "http://[::1]:3000/",
            "http://169.254.169.254/",
            "about:blank",
        ] {
            for op in ["open", "navigate"] {
                let got = outcome(page_op(op, json!({ "url": url })), DEFAULT);
                assert_eq!(got["forward"]["args"]["url"], url);
            }
        }
        for url in [
            "http:/a",
            "http:a",
            "http:\\\\a",
            "httpé://a",
            "hé",
            "https",
            "http:/",
            "xhttp://a",
            "\u{feff}https://a",
            "\u{a0}https://a",
            "\thttps://a",
            "ht\ttps://a",
            "https://a\r\nb",
            "https://a\u{7f}",
            "https://a\u{85}",
            "about:blank ",
            "about:blank?",
            "about:BLANK",
            "ABOUT:blank",
            "about:",
            "about:version",
            "file:///",
            "filesystem:https://a/",
            "javascript:https://a",
            "data:text/html,https://a",
            "herdr-companion://ui/",
            "chrome-extension://a/",
            "chrome-error://chromewebdata/",
            "devtools://devtools/",
            "intent://a",
            "mailto:a@b",
        ] {
            for op in ["open", "navigate"] {
                assert_eq!(
                    rule_of(page_op(op, json!({ "url": url })), DEFAULT),
                    Some(RefuseRule::Url),
                    "{url:?}"
                );
            }
        }
    }

    /// The rows of the sidecar's `isRemoteFile` test (C7): both must agree.
    #[test]
    fn a_shown_file_is_a_token_and_a_relative_path() {
        let show = |token: &str, rel: &str| {
            let args = json!({"token": token, "rel": rel});
            rule_of(link_op("show_remote", None, args), DEFAULT)
        };
        for rel in [
            "index.html",
            "report/in%20dex.html",
            "a/ü.html",
            "..hidden/x%2e",
            "...",
            "%252e%252e/x",
        ] {
            assert_eq!(show(TOKEN, rel), None, "{rel}");
        }
        for rel in [
            "",
            "/",
            "/etc/passwd",
            "//a",
            "..",
            ".",
            "../a",
            "a/../b",
            "a/..",
            "./a",
            "a//b",
            "a/",
            "a\\b",
            "a\nb",
            ".\t./a",
            ".\n.",
            "a\u{1f}",
            "a\u{7f}",
            "a\0",
            "a/%2e%2E/x",
            "%2e/x",
            ".%2E",
            "%2E.",
            "a%5cx",
            "a%5Cx",
            "a%2fx",
            "a%2Fx",
            "x%00",
            "x?y",
            "x#y",
        ] {
            assert_eq!(show(TOKEN, rel), Some(RefuseRule::Argument), "{rel:?}");
        }
        for token in [
            "Abcdefghijklmnopqrstuv12",
            "1bcdefghijklmnopqrstuv12",
            "k3v9x0q2m7w1z8r4t6y5u0abx",
            "k3v9x0q2m7w1z8r4t6y5u0a\n",
        ] {
            assert_eq!(show(token, "a"), Some(RefuseRule::Argument), "{token}");
        }
    }

    fn activity_of(activity: Value) -> Option<Value> {
        let mut op = link_op("tabs", None, Value::Null);
        op.activity = Some(activity);
        let got = outcome(op, DEFAULT);
        assert_eq!(got["forward"]["op"], "tabs", "the op runs");
        got["forward"].get("activity").cloned()
    }

    #[test]
    fn an_activity_is_drawn_by_a() {
        // a minimal one, and herdr's own defaults
        assert_eq!(
            activity_of(json!({"color": "#00c8ff"})),
            Some(json!({"frame": false, "animate": true, "color": "#00c8ff", "linger_ms": 0}))
        );
        assert_eq!(
            activity_of(json!({"color": "#00c8ff", "group": null, "op": "eval"})),
            Some(json!({"frame": false, "animate": true, "color": "#00c8ff", "linger_ms": 0}))
        );
        let long = format!("\u{1b}]0;é\u{9c}\r\n{}", "é".repeat(200));
        let want = format!("]0;{}", "é".repeat(77));
        let max = u64::MAX;
        assert_eq!(
            activity_of(json!({"color": "#00c8ff", "linger_ms": max,
                "group": {"key": long, "title": long, "color": long, "collapse_ms": max}})),
            Some(
                json!({"frame": false, "animate": true, "color": "#00c8ff", "linger_ms": 600_000,
                "group": {"key": want, "title": want, "color": want, "collapse_ms": 600_000}})
            )
        );
        for none in [
            json!(null),
            json!([]),
            json!({}),
            json!(7),
            json!([true, true, "#00c8ff", 1, null]),
            json!({"color": "#00c8ff", "group": ["k", "t", "blue", 1]}),
            json!({"color": "#00c8ff", "group": "k"}),
            json!({"color": "#00c8ff", "group": {"title": ["t"]}}),
            json!({"color": "#00c8ff", "linger_ms": -1}),
            json!({"color": "#00c8ff", "linger_ms": 1.5}),
            json!({"color": "#00c8ff", "linger_ms": "1"}),
            json!({"color": "#00C8FF"}),
            json!({"color": "#00c8ff\n"}),
            json!({"color": ["#00c8ff"]}),
        ] {
            assert_eq!(activity_of(none.clone()), None, "{none}");
        }
    }

    // -----------------------------------------------------------------------
    // Odd JSON

    #[test]
    fn odd_json_is_refused_or_no_frame() {
        // deeper than a JSON reader goes: no frame, which drops the link
        let deep = |n: usize| format!("{}1{}", "[".repeat(n), "]".repeat(n));
        let op = |args: &str| {
            format!(
                r#"{{"t":"op","id":7,"echo":0,"op":"read","target":"9A1F","deadline_ms":1,"args":{args}}}"#
            )
        };
        for args in [
            deep(100_000),
            format!(r#"{{"x":{}}}"#, deep(1_000)),
            r#"{"format":"\ud800"}"#.into(),
            r#"{"format":"a"#.into(),
            r#"{"max":1e999}"#.into(),
        ] {
            assert!(ServerFrame::decode(op(&args).as_bytes()).is_err());
        }
        let read = |args: &str| outcome(decode(&op(args)), DEFAULT);
        let nothing = json!({"forward": {"op": "read", "target": "9A1F"}});
        let refused = |arg: &str| json!({"refuse": {"rule": "argument", "what": arg}});
        for (args, want) in [
            ("null", nothing.clone()),
            ("{}", nothing.clone()),
            (&format!(r#"{{"x":{}}}"#, deep(100)), nothing.clone()),
            (
                r#"{"__proto__":{"format":"html"},"constructor":1,"":0}"#,
                nothing.clone(),
            ),
            // names are exact
            (
                r#"{"Format":7,"format ":7,"for\u0000mat":7}"#,
                nothing.clone(),
            ),
            (r#"{"format":null,"interactive":null}"#, nothing),
            // the last one counts, for the gate and the sidecar alike
            (
                r#"{"format":7,"format":"html"}"#,
                json!({"forward": {"op": "read", "target": "9A1F", "args": {"format": "html"}}}),
            ),
            (r#"{"format":"html","format":7}"#, refused("read format")),
            (
                r#"{"format":"a\u0000b"}"#,
                json!({"forward": {"op": "read", "target": "9A1F", "args": {"format": "a\0b"}}}),
            ),
            (
                &format!(r#"{{"format":{}}}"#, deep(100)),
                refused("read format"),
            ),
            (r#"{"format":{"toString":"html"}}"#, refused("read format")),
            (r#"{"interactive":1}"#, refused("read interactive")),
            (r#"{"interactive":"true"}"#, refused("read interactive")),
            ("[]", refused("read args")),
            (r#"[{"format":"html"}]"#, refused("read args")),
            (r#""{}""#, refused("read args")),
            ("false", refused("read args")),
            ("0", refused("read args")),
        ] {
            assert_eq!(read(args), want, "{args:.80}");
        }
        let of = |op: &str, arg: &str, value: &str| {
            let line = format!(
                r#"{{"t":"op","id":7,"echo":0,"op":"{op}","target":"9A1F","deadline_ms":1,"args":{{"{arg}":{value}}}}}"#
            );
            outcome(decode(&line), DEFAULT).get("refuse").is_none()
        };
        for (value, count, integer) in [
            ("0", true, true),
            ("18446744073709551615", true, true),
            ("18446744073709551616", false, false),
            ("-1", false, true),
            ("-9223372036854775808", false, true),
            ("-9223372036854775809", false, false),
            ("-0", false, false),
            ("1.0", false, false),
            ("1e2", false, false),
            ("\"1\"", false, false),
            ("true", false, false),
            ("[1]", false, false),
            ("{}", false, false),
        ] {
            assert_eq!(of("links", "max", value), count, "{value}");
            assert_eq!(of("scroll", "by", value), integer, "{value}");
        }
    }

    #[test]
    fn a_page_is_named_only_where_the_op_has_one() {
        for rule in OPS.iter().filter(|rule| rule.gate == OpGate::Allowed) {
            let args = match rule.op {
                "show_remote" => json!({"token": TOKEN, "rel": "a"}),
                "open" | "navigate" => json!({"url": "about:blank"}),
                "history" => json!({"action": "back"}),
                "act" => json!({"kind": "click"}),
                "eval" => json!({"expr": "1"}),
                _ => Value::Null,
            };
            let with = outcome(page_op(rule.op, args.clone()), DEFAULT);
            let without = outcome(link_op(rule.op, None, args), DEFAULT);
            if rule.target {
                assert_eq!(with["forward"]["target"], "9A1F", "{}", rule.op);
                let what = format!("{} target", rule.op);
                assert_eq!(
                    without,
                    json!({"refuse": {"rule": "argument", "what": what}})
                );
            } else {
                assert_eq!(with, without, "{}: a page it does not have", rule.op);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Nothing outside the fixtures' allowlist passes

    /// xorshift: the same run every time.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }

        fn chance(&mut self, percent: usize) -> bool {
            self.below(100) < percent
        }

        fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
            &from[self.below(from.len())]
        }
    }

    /// The rules read from the fixture files, not from the tables the gate
    /// uses, with regular expressions instead of its code.
    struct Oracle {
        ops: BTreeMap<String, Value>,
        pressable: BTreeMap<String, String>,
        url: Regex,
        token: Regex,
        rel_segment: Regex,
        rel_bad: Regex,
    }

    impl Oracle {
        fn new() -> Self {
            Self {
                ops: fixture(OPS_FIXTURE)["ops"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|rule| (rule["op"].as_str().unwrap().to_string(), rule.clone()))
                    .collect(),
                pressable: pressable(),
                url: Regex::new(r"\A(?:about:blank|(?i:https?)://[^\p{Cc}]*)\z").unwrap(),
                token: Regex::new(r"\A[a-z][a-z0-9]{23}\z").unwrap(),
                rel_segment: Regex::new(r"\A(?:\.|(?i:%2e)){0,2}\z").unwrap(),
                rel_bad: Regex::new(r"(?i)[\x00-\x1f\x7f\\?#]|%2f|%5c|%00").unwrap(),
            }
        }

        /// A value of a kind (the fixture's name for it) as it goes on.
        fn value(&self, kind: &Value, value: &Value) -> Option<Value> {
            let text = value.as_str();
            let same = |fits: bool| fits.then(|| value.clone());
            if let Some(values) = kind.get("one_of") {
                return same(values.as_array().unwrap().contains(value) && text.is_some());
            }
            match kind.as_str().unwrap() {
                "text" => same(text.is_some()),
                "bool" => same(matches!(value, Value::Bool(_))),
                "count" => same(value.to_string().bytes().all(|b| b.is_ascii_digit())),
                "integer" => same(value.to_string().parse::<i128>().is_ok()),
                "web_url" => same(text.is_some_and(|t| self.url.is_match(t))),
                "eval_text" => same(text.is_some_and(|t| t.len() <= 262_144)),
                "key" => text.and_then(|t| self.pressable.get(t)).map(|t| json!(t)),
                "text_list" => same(
                    value
                        .as_array()
                        .is_some_and(|l| l.len() <= 64 && l.iter().all(|v| v.as_str().is_some())),
                ),
                "file_token" => same(text.is_some_and(|t| self.token.is_match(t))),
                "rel_path" => same(text.is_some_and(|t| {
                    !self.rel_bad.is_match(t) && !t.split('/').any(|s| self.rel_segment.is_match(s))
                })),
                other => panic!("{other}"),
            }
        }

        /// What goes to the sidecar, none if nothing may.
        fn forward(&self, op: &LinkOp, policy: Policy) -> Option<Value> {
            let rule = self.ops.get(&op.op)?;
            if rule["gate"] != "allowed" || (op.op == "eval" && !policy.allow_eval) {
                return None;
            }
            let mut forward = json!({"op": op.op});
            if rule["target"] == true {
                forward["target"] = json!(op.target.as_ref()?);
            }
            if !op.args.is_null() && !op.args.is_object() {
                return None;
            }
            for arg in rule["args"].as_array().unwrap_or(&vec![]) {
                let name = arg["name"].as_str().unwrap();
                if arg["kind"] == "own" {
                    match name {
                        "allow_password" => forward["args"][name] = json!(policy.allow_password),
                        "guard_passwords" => forward["args"][name] = json!(!policy.allow_password),
                        "user" => forward["args"][name] = json!(false),
                        "path" | "inline_path" => {}
                        other => panic!("{other}"),
                    }
                    continue;
                }
                let sent = &op.args[name];
                if !sent.is_null() {
                    forward["args"][name] = self.value(&arg["kind"], sent)?;
                } else if arg["required"] == true || (name == "key" && op.args["kind"] == "press") {
                    return None;
                }
            }
            Some(forward)
        }

        /// Values that fit a kind.
        fn good(&self, kind: &Value) -> Vec<Value> {
            if let Some(values) = kind.get("one_of") {
                return values.as_array().unwrap().clone();
            }
            match kind.as_str().unwrap() {
                "text" => vec![json!(""), json!("e12"), json!("a\u{1b}b")],
                "bool" => vec![json!(true), json!(false)],
                "count" => vec![json!(0), json!(u64::MAX)],
                "integer" => vec![json!(-400), json!(i64::MIN), json!(400)],
                "web_url" => vec![json!("HTTP://a/?b#c"), json!("about:blank")],
                "eval_text" => vec![json!("1+1"), json!("")],
                "key" => vec![json!("Meta+Shift+z"), json!("Shift++"), json!("Enter")],
                "text_list" => vec![json!([]), json!(["w1:p1"]), json!(vec!["a"; 64])],
                "file_token" => vec![json!(TOKEN)],
                "rel_path" => vec![json!("a/b%20c.html")],
                "own" => vec![json!(true), json!("/tmp/x")],
                other => panic!("{other}"),
            }
        }

        /// Near misses of a kind.
        fn bad(&self, kind: &Value) -> Vec<Value> {
            match kind.as_str() {
                Some("web_url") => vec![
                    json!("file:///etc/hosts"),
                    json!("javascript:1"),
                    json!("https://a\nb"),
                    json!(" https://a"),
                    json!("https:/a"),
                ],
                Some("eval_text") => vec![json!("x".repeat(262_145)), json!(["1"])],
                Some("key") => vec![
                    json!("Meta+v"),
                    json!("Shift+Insert"),
                    json!("Shift+Shift+a"),
                    json!("Copy"),
                    json!(13),
                ],
                Some("text_list") => vec![json!(vec!["a"; 65]), json!(["a", null]), json!("a")],
                Some("file_token") => vec![json!("K3v9x0q2m7w1z8r4t6y5u0ab"), json!("")],
                Some("rel_path") => vec![
                    json!("../x"),
                    json!(".\t./x"),
                    json!("a/%2E./x"),
                    json!("/etc/hosts"),
                ],
                Some("count") => vec![json!(-1), json!(0.0), json!("1")],
                Some("integer") => vec![json!(1.5), json!("1")],
                Some("bool") => vec![json!("true"), json!(1)],
                Some("text") => vec![json!(7), json!(["a"]), json!({"a": "b"})],
                // one_of, own (nothing is bad)
                _ => vec![json!("set_input_files"), json!("PRESS"), json!(["back"])],
            }
        }
    }

    #[test]
    fn nothing_outside_the_fixtures_passes() {
        let oracle = Oracle::new();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut op_names: Vec<String> = oracle.ops.keys().cloned().collect();
        for name in op_names.clone() {
            op_names.push(name.to_uppercase());
            op_names.push(format!("{name} "));
            op_names.push(name[1..].to_string());
        }
        op_names.extend(["", "__proto__", "constructor", "toString"].map(String::from));
        let mut arg_names: BTreeSet<String> = oracle
            .ops
            .values()
            .flat_map(|rule| rule["args"].as_array().cloned().unwrap_or_default())
            .map(|arg| arg["name"].as_str().unwrap().to_string())
            .collect();
        for evil in [
            "files",
            "paths",
            "file",
            "root",
            "allow_network",
            "page_api",
            "opener",
            "watch",
            "remote",
            "nonce",
            "port",
            "force",
            "__proto__",
            "KEY",
            "",
        ] {
            arg_names.insert(evil.into());
        }
        let arg_names: Vec<String> = arg_names.into_iter().collect();
        let mut values = vec![
            json!(null),
            json!(true),
            json!(-1),
            json!(1.5),
            json!(1e300),
            json!([]),
            json!([["a"]]),
            json!({}),
            json!({"url": "https://a/"}),
            json!(["a", null]),
            json!("Control+Shift+ArrowLeft"),
        ];
        for rule in oracle.ops.values() {
            for arg in rule["args"].as_array().unwrap_or(&vec![]) {
                for value in [oracle.good(&arg["kind"]), oracle.bad(&arg["kind"])].concat() {
                    // the large one costs time: only for `expr`
                    let large = value.as_str().is_some_and(|text| text.len() > 1_000);
                    if !large && !values.contains(&value) {
                        values.push(value);
                    }
                }
            }
        }
        let allowed: Vec<String> = oracle
            .ops
            .iter()
            .filter(|(_, rule)| rule["gate"] == "allowed")
            .map(|(op, _)| op.clone())
            .collect();
        let mut passed: BTreeMap<String, usize> = BTreeMap::new();
        let mut refused: BTreeMap<String, usize> = BTreeMap::new();
        for _ in 0..40_000 {
            let name = match rng.chance(85) {
                true => rng.pick(&allowed),
                false => rng.pick(&op_names),
            };
            let policy = Policy {
                allow_eval: rng.chance(70),
                allow_password: rng.chance(50),
            };
            let mut args = Map::new();
            if let Some(rules) = oracle
                .ops
                .get(name)
                .and_then(|rule| rule["args"].as_array())
            {
                for rule in rules {
                    let required = rule["required"] == true;
                    if rng.chance(if required { 95 } else { 40 }) {
                        let kind = &rule["kind"];
                        let value = match rng.below(100) {
                            0..80 => rng.pick(&oracle.good(kind)).clone(),
                            80..90 => rng.pick(&oracle.bad(kind)).clone(),
                            _ => rng.pick(&values).clone(),
                        };
                        args.insert(rule["name"].as_str().unwrap().into(), value);
                    }
                }
            }
            for _ in 0..rng.below(4) {
                if rng.chance(30) {
                    args.insert(rng.pick(&arg_names).clone(), rng.pick(&values).clone());
                }
            }
            let mut op = link_op(name, None, Value::Object(args));
            if rng.chance(3) {
                op.args = rng.pick(&values).clone();
            }
            if rng.chance(90) {
                op.target = Some("9A1F".into());
            }
            let want = oracle.forward(&op, policy);
            let shown = format!("{:.400}", serde_json::to_string(&op).unwrap());
            let got = outcome(op, policy);
            match want {
                Some(want) => {
                    assert_eq!(got, json!({ "forward": want }), "{shown}");
                    *passed.entry(name.clone()).or_default() += 1;
                }
                None => {
                    assert_eq!(got.get("forward"), None, "{shown}");
                    let rule = got["refuse"]["rule"].as_str().unwrap_or("helper");
                    *refused.entry(rule.into()).or_default() += 1;
                }
            }
        }
        // the run was no empty one
        assert_eq!(passed.keys().cloned().collect::<Vec<_>>(), allowed);
        assert!(passed.values().all(|n| *n > 100), "{passed:?}");
        assert_eq!(
            refused.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "argument",
                "eval_off",
                "helper",
                "key",
                "local_op",
                "too_large",
                "unknown_op",
                "url"
            ]
        );
        assert!(refused.values().all(|n| *n > 20), "{refused:?}");
    }
}
