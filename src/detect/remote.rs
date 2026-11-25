//! Fork: agents that run on another machine behind a remote-shell client
//! (`ssh`, `mosh`, `et`) in the pane's foreground.
//!
//! Process identification only sees the local client, so the remote
//! command line it was given is the evidence: `ssh -t host 'cd ~/w &&
//! claude'` names `claude` as a command word. Only the command word of
//! the last simple command counts (after `&&`, `||`, `;`, `|`, `&`, `exec`,
//! `env`, assignments), so `ssh host cat claude.md`, `ssh claude@h`,
//! `ssh -i ~/.ssh/claude h` or `ssh -t h 'claude; bash'` (a remote shell
//! outlives the agent) name no agent. A remote token is never resolved
//! against the local file system.
//!
//! An agent found this way is [`RemoteVia::ClientCommand`]; one adopted from
//! its own OSC 7501 status reports while a client is in front is
//! [`RemoteVia::ProgramStatus`] (src/pane/remote_agent.rs). Either way herdr
//! never acts on the local process for it (it is the client): no suspend,
//! restart, resume, handoff or hook-only feature.

use super::Agent;

/// The remote-shell client in the pane's foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteClient {
    Ssh,
    Mosh,
    Et,
}

impl RemoteClient {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Mosh => "mosh",
            Self::Et => "et",
        }
    }
}

/// How a remote agent was identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteVia {
    /// The client's remote command line (`ssh`, `mosh`, `et -c`) runs the
    /// agent.
    ClientCommand,
    /// The agent's own OSC 7501 status reports named it while the client
    /// led the foreground (src/pane/remote_agent.rs).
    ProgramStatus,
}

impl RemoteVia {
    pub fn label(self) -> &'static str {
        match self {
            Self::ClientCommand => "client_command",
            Self::ProgramStatus => "program_status",
        }
    }
}

/// Where an identified agent runs when it is not a local process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteOrigin {
    pub client: RemoteClient,
    pub via: RemoteVia,
    /// The client process the agent was identified behind, filled in when
    /// the identification is published (runtime only, never on the wire):
    /// typed input goes to the foreground only while that same process
    /// leads it (src/app/agents.rs `runtime_hosts_remote_agent`).
    pub client_process: Option<RemoteClientProcess>,
}

/// A client process by pid and start token
/// ([`crate::platform::process_start_token`]), so a reused pid or another
/// client is not taken for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteClientProcess {
    pub pid: u32,
    pub start_token: Option<u64>,
}

impl RemoteOrigin {
    /// This origin pinned to the client process `pid` (reads its start
    /// token: call once per publication, not per probe).
    pub fn with_client_process(self, pid: u32) -> Self {
        Self {
            client_process: Some(RemoteClientProcess {
                pid,
                start_token: crate::platform::process_start_token(pid),
            }),
            ..self
        }
    }
}

/// Nested `sh -c '…'` levels followed inside a remote command.
const MAX_SHELL_DEPTH: u8 = 3;

/// The remote-shell client a process is, from its name or argv[0].
pub fn remote_client(process: &crate::platform::ForegroundProcess) -> Option<RemoteClient> {
    let argv0 = process.argv0.as_deref().or_else(|| {
        process
            .argv
            .as_ref()
            .and_then(|argv| argv.first())
            .map(String::as_str)
    });
    [Some(process.name.as_str()), argv0]
        .into_iter()
        .flatten()
        .find_map(|name| client_from_name(super::path_basename(name)))
}

/// Fork: `argv0` (a path or a name) is a remote-shell client.
pub fn is_remote_client_argv0(argv0: &str) -> bool {
    client_from_name(super::path_basename(argv0)).is_some()
}

fn client_from_name(name: &str) -> Option<RemoteClient> {
    let name = name.to_ascii_lowercase();
    // Windows OpenSSH runs as `ssh.exe`.
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    match name {
        "ssh" => Some(RemoteClient::Ssh),
        "mosh" | "mosh-client" => Some(RemoteClient::Mosh),
        "et" => Some(RemoteClient::Et),
        _ => None,
    }
}

/// The agent a remote-shell client process runs on the other side, read
/// from its argv.
pub fn remote_agent_from_process(
    process: &crate::platform::ForegroundProcess,
) -> Option<(Agent, RemoteClient)> {
    let client = remote_client(process)?;
    let argv = process.argv.as_deref()?;
    let command = remote_command(client, argv)?;
    agent_in_command_line(&command).map(|agent| (agent, client))
}

/// The command line a client asks the remote side to run, or `None` for an
/// interactive login (or a client invocation that runs no command).
fn remote_command(client: RemoteClient, argv: &[String]) -> Option<String> {
    let name = argv
        .first()
        .map(|arg| super::path_basename(arg).to_ascii_lowercase());
    match client {
        RemoteClient::Ssh => ssh_remote_command(argv.get(1..)?),
        RemoteClient::Mosh if name.as_deref() == Some("mosh-client") => {
            mosh_client_remote_command(argv.get(1..)?)
        }
        RemoteClient::Mosh => mosh_remote_command(argv.get(1..)?, false),
        RemoteClient::Et => et_remote_command(argv.get(1..)?),
    }
}

/// OpenSSH's option letters that take a value (`ssh.c` optstring).
fn ssh_option_takes_value(option: char) -> bool {
    "bceilmopwBDEFIJLOPQRSW".contains(option)
}

/// OpenSSH parses options, takes the destination, then parses options
/// again (`ssh host -p 22 cmd` works); the first other word starts the
/// command, and every remaining word is joined with spaces, as ssh sends it.
fn ssh_remote_command(args: &[String]) -> Option<String> {
    let mut host = None;
    let mut remote_command_option = None;
    let mut index = 0;
    let mut options_done = false;
    while index < args.len() {
        let arg = &args[index];
        if !options_done && arg == "--" {
            options_done = true;
            index += 1;
            continue;
        }
        if !options_done && arg.len() > 1 && arg.starts_with('-') {
            let letters: Vec<char> = arg[1..].chars().collect();
            let mut position = 0;
            while position < letters.len() {
                let option = letters[position];
                if matches!(option, 'N' | 'W' | 'O' | 'G' | 'V' | 'Q') {
                    // No remote command: a forward-only session, a
                    // control request or a query.
                    return None;
                }
                if ssh_option_takes_value(option) {
                    let attached: String = letters[position + 1..].iter().collect();
                    let value = if attached.is_empty() {
                        index += 1;
                        args.get(index).cloned().unwrap_or_default()
                    } else {
                        attached
                    };
                    if option == 'o' {
                        if let Some(command) = ssh_remote_command_option(&value) {
                            remote_command_option = Some(command);
                        }
                    }
                    break;
                }
                position += 1;
            }
            index += 1;
            continue;
        }
        if host.is_none() {
            host = Some(arg);
            // Options may follow the destination until the command starts.
            options_done = false;
            index += 1;
            continue;
        }
        return Some(args[index..].join(" "));
    }
    host?;
    remote_command_option
}

/// `-o RemoteCommand=…` (or `RemoteCommand …`), which runs when no
/// command is given on the command line.
fn ssh_remote_command_option(value: &str) -> Option<String> {
    let value = value.trim_start();
    let split = value.find(|ch: char| ch == '=' || ch.is_whitespace())?;
    let (key, rest) = value.split_at(split);
    if !key.eq_ignore_ascii_case("RemoteCommand") {
        return None;
    }
    let command = rest
        .trim_start_matches(|ch: char| ch.is_whitespace())
        .trim_start_matches('=')
        .trim();
    (!command.is_empty() && !command.eq_ignore_ascii_case("none")).then(|| command.to_string())
}

/// `mosh [options] [--] host [command…]`. `split_values`: the words were
/// re-split from text that lost its quoting, so a `--ssh "ssh -J jump"`
/// value now reads as words of their own; OpenSSH's value options and
/// their values are skipped too, so `jump` is not taken for the host.
fn mosh_remote_command(args: &[String], split_values: bool) -> Option<String> {
    let mut index = 0;
    let mut host = None;
    while index < args.len() {
        let arg = &args[index];
        if host.is_none() && arg == "--" {
            index += 1;
            continue;
        }
        if host.is_none() && arg.starts_with('-') {
            let takes_value = matches!(
                arg.as_str(),
                "-p" | "--port"
                    | "--client"
                    | "--server"
                    | "--ssh"
                    | "--predict"
                    | "--family"
                    | "--bind-server"
                    | "--experimental-remote-ip"
            ) || (split_values && is_ssh_value_option(arg));
            index += if takes_value { 2 } else { 1 };
            continue;
        }
        if host.is_none() {
            host = Some(arg);
            index += 1;
            if args.get(index).is_some_and(|arg| arg == "--") {
                index += 1;
            }
            continue;
        }
        return Some(args[index..].join(" "));
    }
    None
}

/// The `mosh` script execs `mosh-client "-# <its own args> |" ip port`;
/// the `-#` text (joined with spaces, so quoting is lost) is all that is
/// left of the command.
fn mosh_client_remote_command(args: &[String]) -> Option<String> {
    let original = args.iter().find_map(|arg| arg.strip_prefix("-#"))?;
    let original = original.trim();
    let original = original.strip_suffix('|').unwrap_or(original).trim();
    let words: Vec<String> = original.split_whitespace().map(str::to_string).collect();
    mosh_remote_command(&words, true)
}

/// A lone OpenSSH option letter that takes the next word as its value
/// (`-J`, `-p`, `-o`).
fn is_ssh_value_option(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars.next() == Some('-')
        && chars
            .next()
            .is_some_and(|option| ssh_option_takes_value(option))
        && chars.next().is_none()
}

/// `et [options] host` runs a command only with `-c` / `--command`.
fn et_remote_command(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "-c" || arg == "--command" {
            return iter.next().cloned();
        }
        if let Some(command) = arg.strip_prefix("--command=") {
            return Some(command.to_string());
        }
        if let Some(command) = arg.strip_prefix("-c").filter(|rest| !rest.is_empty()) {
            if !arg.starts_with("--") {
                return Some(command.to_string());
            }
        }
    }
    None
}

/// The agent a shell command line runs as the command word of its last
/// simple command. An agent that runs earlier (`claude; bash`,
/// `claude || exec $SHELL`) leaves a remote shell in front once it exits,
/// which the client's command line cannot tell apart, so it names nothing.
fn agent_in_command_line(command: &str) -> Option<Agent> {
    agent_in_command_line_at(command, 0)
}

fn agent_in_command_line_at(command: &str, depth: u8) -> Option<Agent> {
    let tokens = shell_tokens(command);
    let last = tokens
        .split(|token| matches!(token, ShellToken::Operator))
        .map(|command| {
            command
                .iter()
                .filter_map(|token| match token {
                    ShellToken::Word(word) => Some(word.as_str()),
                    ShellToken::Operator => None,
                })
                .collect::<Vec<&str>>()
        })
        // A closing keyword ends the compound command whose body ran last
        // (`{ claude; }`, `if …; then claude; fi`).
        .rfind(|words| {
            !words.is_empty() && !matches!(words.as_slice(), ["}" | "fi" | "done" | "esac"])
        })?;
    agent_in_simple_command(&last, depth)
}

/// One simple command's words: skip the prefixes that run the next word
/// as the command, then match that word.
fn agent_in_simple_command(words: &[&str], depth: u8) -> Option<Agent> {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        if is_assignment(word) {
            index += 1;
            continue;
        }
        match *word {
            "exec" | "command" | "nohup" | "builtin" | "time" | "then" | "do" | "else" | "{"
            | "!" | "(" => {
                index += 1;
                // `command -v claude` / `command -V claude` only look it up.
                if *word == "command"
                    && words[index..]
                        .iter()
                        .take_while(|option| option.starts_with('-'))
                        .any(|option| option[1..].contains(['v', 'V']))
                {
                    return None;
                }
                // `exec -a name cmd`, `command -p cmd`.
                while let Some(option) = words.get(index).filter(|word| word.starts_with('-')) {
                    index += if *option == "-a" { 2 } else { 1 };
                }
                continue;
            }
            "env" => {
                index += 1;
                while let Some(arg) = words.get(index) {
                    if is_assignment(arg) {
                        index += 1;
                    } else if matches!(*arg, "-u" | "--unset" | "-C" | "--chdir" | "-S") {
                        index += 2;
                    } else if arg.starts_with('-') {
                        index += 1;
                    } else {
                        break;
                    }
                }
                continue;
            }
            _ => {}
        }
        if let Some(script) = shell_command_string(word, &words[index + 1..]) {
            return (depth < MAX_SHELL_DEPTH)
                .then(|| agent_in_command_line_at(script, depth + 1))
                .flatten();
        }
        return remote_agent_word(word);
    }
    None
}

/// `sh -c '…'`, `bash -lc '…'`, `zsh -ic '…'`: the script a shell runs.
fn shell_command_string<'a>(command: &str, args: &[&'a str]) -> Option<&'a str> {
    let shell = super::path_basename(command);
    if !matches!(shell, "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish") {
        return None;
    }
    let mut runs_script = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if *arg == "--" {
            break;
        }
        if arg.starts_with("--") {
            continue;
        }
        if let Some(flags) = arg.strip_prefix(['-', '+']) {
            if arg.starts_with('-') && flags.contains('c') {
                runs_script = true;
            }
            // `-o pipefail`, `+o posix`, `-euo pipefail`, `-O extglob`: the
            // option name is the next word.
            if flags.ends_with(['o', 'O']) {
                args.next();
            }
            continue;
        }
        return runs_script.then_some(*arg);
    }
    None
}

/// A remote command word names an agent by its basename only; a remote
/// path is never resolved locally.
fn remote_agent_word(word: &str) -> Option<Agent> {
    let basename = super::path_basename(word);
    if basename.is_empty() || basename.starts_with('-') || basename.contains('=') {
        return None;
    }
    super::parse_agent_label(basename)
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

#[derive(Debug, PartialEq, Eq)]
enum ShellToken {
    Word(String),
    /// `&&`, `||`, `;`, `|`, `&`, a newline, or a parenthesis: a new
    /// simple command starts after it.
    Operator,
}

/// A small POSIX-shell word splitter: quotes and backslashes are honoured,
/// control operators end a simple command. Expansions are left as text,
/// including a command substitution (`$(…)`, `` `…` ``), whose commands
/// run inside the word, not as commands of their own.
fn shell_tokens(command: &str) -> Vec<ShellToken> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    let finish = |word: &mut String, in_word: &mut bool, tokens: &mut Vec<ShellToken>| {
        if *in_word {
            tokens.push(ShellToken::Word(std::mem::take(word)));
            *in_word = false;
        }
    };
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                in_word = true;
                for inner in chars.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                    word.push(inner);
                }
            }
            '"' => {
                in_word = true;
                while let Some(inner) = chars.next() {
                    match inner {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                if !matches!(escaped, '"' | '\\' | '$' | '`') {
                                    word.push('\\');
                                }
                                word.push(escaped);
                            }
                        }
                        _ => word.push(inner),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(escaped) = chars.next() {
                    if escaped != '\n' {
                        word.push(escaped);
                    }
                }
            }
            '$' if chars.peek() == Some(&'(') => {
                in_word = true;
                word.push(ch);
                let mut depth = 0usize;
                for inner in chars.by_ref() {
                    word.push(inner);
                    match inner {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            '`' => {
                in_word = true;
                word.push(ch);
                while let Some(inner) = chars.next() {
                    word.push(inner);
                    match inner {
                        '`' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                word.push(escaped);
                            }
                        }
                        _ => {}
                    }
                }
            }
            // `2>&1`, `>&2`, `&>log`: a redirection, not a control operator.
            '&' if (in_word && word.ends_with(['>', '<'])) || chars.peek() == Some(&'>') => {
                in_word = true;
                word.push(ch);
            }
            '#' if !in_word => {
                // A comment runs to the end of the line.
                for inner in chars.by_ref() {
                    if inner == '\n' {
                        tokens.push(ShellToken::Operator);
                        break;
                    }
                }
            }
            ';' | '&' | '|' | '\n' | '(' | ')' => {
                finish(&mut word, &mut in_word, &mut tokens);
                if matches!(ch, '&' | '|') && chars.peek() == Some(&ch) {
                    chars.next();
                }
                tokens.push(ShellToken::Operator);
            }
            ch if ch.is_whitespace() => finish(&mut word, &mut in_word, &mut tokens),
            ch => {
                in_word = true;
                word.push(ch);
            }
        }
    }
    finish(&mut word, &mut in_word, &mut tokens);
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    fn process(name: &str, args: &[&str]) -> crate::platform::ForegroundProcess {
        crate::platform::ForegroundProcess {
            pid: 7,
            name: name.to_string(),
            argv0: None,
            argv: Some(argv(args)),
            cmdline: Some(args.join(" ")),
        }
    }

    fn ssh(args: &[&str]) -> Option<Agent> {
        let mut full = vec!["ssh"];
        full.extend_from_slice(args);
        remote_agent_from_process(&process("ssh", &full)).map(|(agent, _)| agent)
    }

    #[test]
    fn a_quoted_remote_command_runs_the_agent_after_cd() {
        assert_eq!(
            ssh(&["-t", "host", "cd ~/work && claude"]),
            Some(Agent::Claude)
        );
    }

    #[test]
    fn remote_commands_that_run_an_agent() {
        let cases: &[&[&str]] = &[
            &["host", "claude"],
            &["-t", "host", "claude", "--resume", "0b1c"],
            &["-tt", "host", "claude"],
            &["-p2222", "host", "claude"],
            &["-p", "2222", "-i", "~/.ssh/id", "host", "claude"],
            &["-tp", "2222", "host", "claude"],
            &[
                "-J",
                "jump",
                "-o",
                "ServerAliveInterval=30",
                "host",
                "claude",
            ],
            &["host", "-p", "22", "claude"],
            &["--", "host", "claude"],
            &["user@host", "cd /srv; claude"],
            &["host", "cd ~/w || exit 1; exec claude --continue"],
            &["host", "FOO=1 BAR=x claude"],
            &["host", "env -u TERM FOO=1 claude"],
            &["host", "source ~/.profile && /opt/bin/claude"],
            &["host", "bash -lc 'cd ~/w && claude'"],
            &["host", "zsh", "-ic", "'claude --resume'"],
            &["host", "nohup claude"],
            &["-o", "RemoteCommand=cd ~/w && claude", "host"],
            &["-oRemoteCommand claude", "host"],
        ];
        for args in cases {
            assert_eq!(ssh(args), Some(Agent::Claude), "ssh {args:?}");
        }
        assert_eq!(ssh(&["host", "codex"]), Some(Agent::Codex));
        assert_eq!(ssh(&["host", "cd x && codex resume"]), Some(Agent::Codex));
    }

    #[test]
    fn ssh_invocations_that_run_no_agent() {
        let cases: &[&[&str]] = &[
            &[],
            &["host"],
            &["pi"],
            &["claude@host"],
            &["claude"],
            &["-l", "claude", "host"],
            &["-lclaude", "host"],
            &["-i", "~/.ssh/claude", "host"],
            &["-o", "ProxyCommand=nc claude 22", "host"],
            &["-F", "claude", "host"],
            &["host", "echo claude"],
            &["host", "cat", "claude.md"],
            &["host", "tmux new -s claude"],
            &["host", "grep -r claude ."],
            &["host", "cd claude && ls"],
            &["host", "vim ~/claude/notes"],
            &["host", "echo 'x && claude'"],
            &["host", "# claude"],
            &["-N", "-L", "8080:localhost:80", "host"],
            &["-W", "claude:22", "jump"],
            &["-O", "exit", "host"],
            &["-o", "RemoteCommand=none", "host"],
            &["host", "claude-notes"],
            &["host", "--", "./claude.sh"],
        ];
        for args in cases {
            assert_eq!(ssh(args), None, "ssh {args:?}");
        }
    }

    #[test]
    fn only_remote_shell_clients_are_read() {
        assert_eq!(
            remote_agent_from_process(&process("scp", &["scp", "host:claude", "."])),
            None
        );
        assert_eq!(
            remote_agent_from_process(&process("zsh", &["zsh", "-c", "claude"])),
            None
        );
        assert_eq!(
            remote_agent_from_process(&process("ssh", &["/usr/bin/ssh", "h", "claude"])),
            Some((Agent::Claude, RemoteClient::Ssh))
        );
    }

    #[test]
    fn mosh_and_et_commands() {
        assert_eq!(
            remote_agent_from_process(&process(
                "mosh-client",
                &[
                    "mosh-client",
                    "-# host -- sh -c cd ~/w && claude |",
                    "10.0.0.2",
                    "60001",
                ]
            )),
            Some((Agent::Claude, RemoteClient::Mosh))
        );
        assert_eq!(
            remote_agent_from_process(&process(
                "mosh-client",
                &["mosh-client", "-# host |", "10.0.0.2", "60001",]
            )),
            None
        );
        assert_eq!(
            remote_agent_from_process(&process(
                "perl",
                &["mosh", "--ssh", "ssh -p 22", "host", "--", "claude"]
            )),
            Some((Agent::Claude, RemoteClient::Mosh))
        );
        assert_eq!(
            remote_agent_from_process(&process("et", &["et", "host", "-c", "cd w && claude"])),
            Some((Agent::Claude, RemoteClient::Et))
        );
        assert_eq!(
            remote_agent_from_process(&process("et", &["et", "-p", "2022", "claude"])),
            None
        );
    }

    #[test]
    fn only_an_agent_in_the_last_command_counts() {
        // The remote shell outlives an agent that runs before it.
        for command in [
            "claude; bash",
            "claude || exec $SHELL",
            "claude && bash -l",
            "claude | tee log",
        ] {
            assert_eq!(ssh(&["-t", "host", command]), None, "{command}");
        }
        for command in [
            "cd w && claude",
            "claude;",
            "claude\n",
            "claude # resume later",
            "claude 2>&1",
            "claude &>log",
            "{ claude; }",
            "bash -lc 'claude; true' && cd w && claude",
        ] {
            assert_eq!(
                ssh(&["-t", "host", command]),
                Some(Agent::Claude),
                "{command}"
            );
        }
    }

    #[test]
    fn a_command_substitution_runs_no_command_of_its_own() {
        for command in [
            "V=$(claude --version); bash",
            "echo $(claude --version)",
            "echo `ls; claude --version`",
            "echo $(cd w; claude)",
        ] {
            assert_eq!(ssh(&["host", command]), None, "{command}");
        }
        assert_eq!(ssh(&["host", "cd $(pwd)/w && claude"]), Some(Agent::Claude));
    }

    #[test]
    fn command_v_only_looks_the_agent_up() {
        for command in [
            "command -v claude",
            "command -V claude",
            "command -pv claude",
        ] {
            assert_eq!(ssh(&["host", command]), None, "{command}");
        }
        assert_eq!(ssh(&["host", "command -p claude"]), Some(Agent::Claude));
    }

    #[test]
    fn shell_option_values_are_not_the_script() {
        for command in [
            "bash -o pipefail -c 'claude'",
            "bash -euo pipefail -c 'claude'",
            "bash +o posix -c 'claude'",
            "bash -O extglob -c 'claude'",
        ] {
            assert_eq!(ssh(&["host", command]), Some(Agent::Claude), "{command}");
        }
        assert_eq!(ssh(&["host", "bash -o pipefail claude.sh"]), None);
    }

    #[test]
    fn windows_openssh_is_a_remote_client() {
        assert_eq!(
            remote_agent_from_process(&process(
                "ssh.exe",
                &["C:\\Windows\\System32\\OpenSSH\\ssh.exe", "host", "claude"]
            )),
            Some((Agent::Claude, RemoteClient::Ssh))
        );
        assert_eq!(client_from_name("SSH.EXE"), Some(RemoteClient::Ssh));
        assert!(is_remote_client_argv0("/usr/bin/ssh"));
        assert!(!is_remote_client_argv0("claude"));
    }

    #[test]
    fn mosh_client_text_skips_ssh_options_inside_the_ssh_value() {
        // `mosh --ssh "ssh -J jump" pi`: the quoting is gone in `-#`.
        assert_eq!(
            remote_agent_from_process(&process(
                "mosh-client",
                &[
                    "mosh-client",
                    "-# --ssh ssh -J jump pi |",
                    "10.0.0.2",
                    "60001"
                ]
            )),
            None
        );
        assert_eq!(
            remote_agent_from_process(&process(
                "mosh-client",
                &[
                    "mosh-client",
                    "-# --ssh ssh -J jump host claude |",
                    "10.0.0.2",
                    "60001"
                ]
            )),
            Some((Agent::Claude, RemoteClient::Mosh))
        );
        assert_eq!(
            remote_agent_from_process(&process(
                "mosh-client",
                &["mosh-client", "-# host claude |", "10.0.0.2", "60001"]
            )),
            Some((Agent::Claude, RemoteClient::Mosh))
        );
    }

    #[test]
    fn the_tokenizer_honours_quotes_and_operators() {
        assert_eq!(
            shell_tokens(r#"a "b c"&&d\ e|f"#),
            vec![
                ShellToken::Word("a".into()),
                ShellToken::Word("b c".into()),
                ShellToken::Operator,
                ShellToken::Word("d e".into()),
                ShellToken::Operator,
                ShellToken::Word("f".into()),
            ]
        );
    }
}
