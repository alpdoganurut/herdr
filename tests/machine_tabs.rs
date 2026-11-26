//! Fork (machine tabs): one client, two real servers, the second one a saved
//! SSH machine behind a private `ssh` (the saved-machine discovery and the
//! stdio bridge run for real, against a disposable server). What it asserts
//! is read off the client's screen.
//!
//! Both servers number their tabs and panes alike, and their notes keys
//! follow the tab ids, so nothing here tells the machines apart but what
//! each was told to print. The servers end through `server.stop`.

#![cfg(unix)]

pub mod support;
#[path = "support/terminal_screen.rs"]
mod terminal_screen;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use support::{wait_for_socket, wait_until};

const COLS: u16 = 120;
const ROWS: u16 = 34;
const PROFILE: &str = "0123456789abcdef0123456789abcdef";
/// Only the failure path waits this long.
const READY: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(50);

fn app_dir() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

/// Every variable a Herdr pane or a user's shell would pass on.
fn isolate(command: &mut CommandBuilder, home: &Path) {
    support::isolate_herdr_test_process(command);
    for name in [
        "HERDR_CLIENT_SOCKET_PATH",
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_TAB_ID",
        "HERDR_WORKSPACE_ID",
        "HERDR_CONFIG_PATH",
    ] {
        command.env_remove(name);
    }
    command.env("HOME", home.join("home"));
    command.env("XDG_CONFIG_HOME", home.join("config"));
    command.env("XDG_STATE_HOME", home.join("state"));
    command.env("XDG_RUNTIME_DIR", home.join("run"));
    command.env("HERDR_SOCKET_PATH", home.join("run/herdr.sock"));
    command.env("HERDR_DISABLE_SOUND", "1");
    command.env("SHELL", "/bin/sh");
}

fn spawn(command: CommandBuilder) -> (Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let child = pair.slave.spawn_command(command).unwrap();
    (pair.master, child)
}

/// One machine: its own home, config, state and server.
struct Machine {
    home: PathBuf,
    running: Option<(Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>)>,
}

impl Machine {
    fn new(home: PathBuf) -> Self {
        for dir in ["home", "state", "run"] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        let config = home.join("config").join(app_dir());
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("config.toml"),
            "onboarding = false\n\
             [ui]\nsidebar_layout = \"tabs\"\n\
             [ui.toast]\ndelivery = \"herdr\"\n\
             [keys]\ntoggle_info_pane = \"prefix+i\"\n\
             [browser]\nremote_machines = false\n",
        )
        .unwrap();
        let mut machine = Self {
            home,
            running: None,
        };
        machine.start();
        machine
    }

    fn socket(&self) -> PathBuf {
        self.home.join("run/herdr.sock")
    }

    fn start(&mut self) {
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
        isolate(&mut command, &self.home);
        command.arg("server");
        self.running = Some(spawn(command));
        wait_for_socket(&self.socket(), Duration::from_secs(20));
        wait_for_socket(&self.home.join("run/herdr-client.sock"), READY);
    }

    /// Ask the server to stop and wait for it to be gone.
    fn stop(&mut self) -> bool {
        let Some((_master, mut child)) = self.running.take() else {
            return true;
        };
        if let Ok(mut stream) = UnixStream::connect(self.socket()) {
            let _ = writeln!(
                stream,
                r#"{{"id":"stop","method":"server.stop","params":{{}}}}"#
            );
            let mut line = String::new();
            let _ = BufReader::new(stream).read_line(&mut line);
        }
        wait_until(Duration::from_secs(15), POLL, || {
            !matches!(child.try_wait(), Ok(None))
        })
    }

    /// Off the network: no new connection. (Without this the client's next
    /// attempt would start the machine's server again.)
    fn unplug(&self) {
        fs::write(self.home.join("unplugged"), "").unwrap();
    }

    fn plug_in(&self) {
        fs::remove_file(self.home.join("unplugged")).unwrap();
    }

    fn api(&self, method: &str, params: Value) -> Value {
        let mut stream = UnixStream::connect(self.socket()).expect("the API socket");
        writeln!(
            stream,
            "{}",
            json!({"id": method, "method": method, "params": params})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).expect("a JSON reply");
        assert!(reply.get("error").is_none(), "{method}: {reply}");
        reply["result"].clone()
    }

    /// Print `text` in `pane` (its command line has it in two parts).
    fn print(&self, pane: &str, text: &str) {
        let (head, tail) = text.split_at(text.len() / 2);
        self.api(
            "pane.send_input",
            json!({"pane_id": pane, "text": format!("printf '%s%s\\n' {head} {tail}"), "keys": ["Enter"]}),
        );
    }

    /// `<name>-one` and `<name>-two`, each with `<NAME>_ONE` / `_TWO` printed
    /// in its pane; the first focused. Returns their panes.
    fn two_tabs(&self, name: &str) -> [String; 2] {
        let created = self.api(
            "workspace.create",
            json!({"cwd": self.home.join("home"), "focus": true, "label": format!("{name}-space")}),
        );
        let first = created["root_pane"]["pane_id"].as_str().unwrap().to_owned();
        let tab = created["tab"]["tab_id"].as_str().unwrap().to_owned();
        self.api(
            "tab.rename",
            json!({"tab_id": tab, "label": format!("{name}-one")}),
        );
        let created = self.api(
            "tab.create",
            json!({
                "workspace_id": created["workspace"]["workspace_id"],
                "focus": false,
                "label": format!("{name}-two"),
            }),
        );
        let second = created["root_pane"]["pane_id"].as_str().unwrap().to_owned();
        let name = name.to_uppercase();
        self.print(&first, &format!("{name}_ONE"));
        self.print(&second, &format!("{name}_TWO"));
        [first, second]
    }

    /// Every notes page on this machine.
    fn notes(&self) -> Vec<(String, String)> {
        let dir = self.home.join("config").join(app_dir()).join("notes");
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read_to_string(entry.path()).unwrap_or_default(),
                )
            })
            .collect()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = fs::write(self.home.join("unplugged"), "");
        self.stop();
    }
}

/// The client in a terminal of `COLS` x `ROWS`.
struct Client {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
    input: Option<Box<dyn Write + Send>>,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Client {
    fn start(home: &Path, path: &str) -> Self {
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
        isolate(&mut command, home);
        command.env("PATH", path);
        command.arg("client");
        let (master, child) = spawn(command);
        let mut reader = master.try_clone_reader().unwrap();
        let input = master.take_writer().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = output.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(read @ 1..) = reader.read(&mut buffer) {
                sink.lock().unwrap().extend_from_slice(&buffer[..read]);
            }
        });
        Self {
            master: Some(master),
            child,
            input: Some(input),
            output,
        }
    }

    fn screen(&self) -> Vec<String> {
        let bytes = self.output.lock().unwrap().clone();
        terminal_screen::text(&bytes, COLS, ROWS)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The styled cells, as escape sequences.
    fn styled(&self) -> Vec<String> {
        let bytes = self.output.lock().unwrap().clone();
        terminal_screen::styled(&bytes, COLS, ROWS)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The machine row: the sidebar's part of the first line.
    fn row(&self) -> String {
        let screen = self.screen();
        let first = screen.first().cloned().unwrap_or_default();
        first
            .split('│')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    }

    fn shows(&self, text: &str) -> bool {
        self.screen().iter().any(|line| line.contains(text))
    }

    #[track_caller]
    fn wait(&self, what: &str, done: impl Fn(&Self) -> bool) {
        assert!(
            wait_until(READY, POLL, || done(self)),
            "{what}:\n{}",
            self.screen().join("\n")
        );
    }

    /// For `time`, what is on screen keeps holding.
    #[track_caller]
    fn holds(&self, what: &str, time: Duration, check: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + time;
        while Instant::now() < deadline {
            assert!(check(self), "{what}:\n{}", self.screen().join("\n"));
            thread::sleep(POLL);
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().expect("the client's terminal");
        input.write_all(bytes).unwrap();
        input.flush().unwrap();
    }

    /// prefix, then `key`.
    fn prefix(&mut self, key: char) {
        self.send(b"\x02");
        thread::sleep(Duration::from_millis(60));
        self.send(key.to_string().as_bytes());
    }

    fn click(&mut self, column: usize, row: usize) {
        self.send(format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").as_bytes());
    }

    /// Click `text` in the first `columns` cells of any row from `from` on.
    #[track_caller]
    fn click_text(&mut self, text: &str, from: usize, columns: usize) {
        let screen = self.screen();
        let (row, column) = screen
            .iter()
            .enumerate()
            .skip(from)
            .find_map(|(row, line)| {
                let byte = line.find(text)?;
                let column = line[..byte].chars().count();
                (column < columns).then_some((row, column))
            })
            .unwrap_or_else(|| panic!("{text:?} is not on screen:\n{}", screen.join("\n")));
        self.click(column + 1, row + 1);
    }

    fn click_machine(&mut self, label: &str) {
        self.click_text(label, 0, 36);
    }

    /// The machine whose tabs and pane are shown.
    fn on(&self, name: &str) -> bool {
        let other = if name == "alpha" { "beta" } else { "alpha" };
        let sidebar: Vec<String> = self
            .screen()
            .iter()
            .skip(1)
            .map(|line| line.split('│').next().unwrap_or_default().to_owned())
            .collect();
        let lists = |label: &str| sidebar.iter().any(|line| line.contains(label));
        lists(&format!("{name}-one")) && !lists(&format!("{other}-one"))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Detach; a client whose terminal is gone ends too.
        if let Some(input) = self.input.as_mut() {
            let _ = input.write_all(b"\x02");
            thread::sleep(Duration::from_millis(60));
            let _ = input.write_all(b"q");
        }
        wait_until(Duration::from_secs(3), POLL, || {
            !matches!(self.child.try_wait(), Ok(None))
        });
        drop(self.input.take());
        drop(self.master.take());
        wait_until(Duration::from_secs(5), POLL, || {
            !matches!(self.child.try_wait(), Ok(None))
        });
    }
}

/// A faint (SGR 2) run in a styled line.
fn has_dim(line: &str) -> bool {
    line.split("\x1b[")
        .filter_map(|run| run.split_once('m').map(|(sgr, _)| sgr))
        .any(|sgr| {
            let mut codes = sgr.split(';');
            while let Some(code) = codes.next() {
                match code {
                    "2" => return true,
                    // a colour: its arguments are no attributes
                    "38" | "48" | "58" => match codes.next() {
                        Some("2") => {
                            codes.by_ref().take(3).for_each(drop);
                        }
                        Some("5") => drop(codes.next()),
                        _ => {}
                    },
                    _ => {}
                }
            }
            false
        })
}

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(PathBuf::from(format!(
            "/tmp/hmt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// `ssh` for the client alone: it runs the remote command here, in the other
/// machine's environment, while that machine is not `unplug`ged.
fn write_ssh(bin: &Path, machine: &Path) {
    fs::create_dir_all(bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_herdr"), bin.join("herdr")).unwrap();
    fs::write(
        bin.join("ssh"),
        format!(
            "#!/bin/sh\n\
             export HOME={home} XDG_CONFIG_HOME={config} XDG_STATE_HOME={state} XDG_RUNTIME_DIR={run} HERDR_SOCKET_PATH={socket}\n\
             unset HERDR_CLIENT_SOCKET_PATH HERDR_SESSION\n\
             control=; previous=; configuration=\n\
             for arg do\n\
             \x20 if [ \"$previous\" = -S ]; then control=$arg; fi\n\
             \x20 if [ \"$arg\" = -G ]; then configuration=yes; fi\n\
             \x20 previous=$arg; last=$arg\n\
             done\n\
             if [ -n \"$configuration\" ]; then printf 'controlpath %s\\n' \"$control\"; exit 0; fi\n\
             if [ -e {unplugged} ]; then exit 255; fi\n\
             exec /bin/sh -c \"$last\"\n",
            unplugged = quote(&machine.join("unplugged")),
            home = quote(&machine.join("home")),
            config = quote(&machine.join("config")),
            state = quote(&machine.join("state")),
            run = quote(&machine.join("run")),
            socket = quote(&machine.join("run/herdr.sock")),
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("ssh"), fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn two_real_machines_share_one_client() {
    let dir = TestDir::new();
    let local = Machine::new(dir.0.join("a"));
    let mut build = Machine::new(dir.0.join("b"));
    let [_, _] = local.two_tabs("alpha");
    let [beta_one, beta_two] = build.two_tabs("beta");

    // A selection stored by an older client: the tabs layout starts on Local.
    let catalog = local.home.join("state").join(app_dir()).join("client");
    fs::create_dir_all(&catalog).unwrap();
    fs::write(
        catalog.join("endpoints.json"),
        json!({
            "version": 1, "selected_profile": PROFILE,
            "ssh": [{"id": PROFILE, "label": "Build", "target": "test-only", "session": "default", "enabled": true}],
        })
        .to_string(),
    )
    .unwrap();
    let bin = dir.0.join("bin");
    write_ssh(&bin, &build.home);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut client = Client::start(&local.home, &path);

    // The row appears, on Local.
    client.wait("both machines are connected, Local is shown", |client| {
        client.row() == "○ Local  ○ Build" && client.on("alpha") && client.shows("ALPHA_ONE")
    });

    client.holds("it stays on Local", Duration::from_secs(3), |client| {
        client.on("alpha")
    });

    // A click switches the sidebar, the header and the pane.
    client.click_machine("Build");
    client.wait("a click shows Build", |client| {
        client.on("beta") && client.shows("BETA_ONE") && !client.shows("ALPHA_ONE")
    });
    client.click_text("beta-two", 1, 36);
    client.wait("Build's second tab", |client| client.shows("BETA_TWO"));

    // The key steps through the machines; each keeps its tab.
    client.prefix('m');
    client.wait("prefix+m shows Local", |client| {
        client.on("alpha") && client.shows("ALPHA_ONE")
    });
    client.click_text("alpha-two", 1, 36);
    client.wait("Local's second tab", |client| client.shows("ALPHA_TWO"));
    client.prefix('m');
    client.wait("Build comes back on its second tab", |client| {
        client.on("beta") && client.shows("BETA_TWO")
    });
    client.click_machine("Local");
    client.wait("Local comes back on its second tab", |client| {
        client.on("alpha") && client.shows("ALPHA_TWO")
    });

    // An agent of the machine not shown waits: its mark, and a toast that
    // names it.
    let report = |state: &str| {
        build.api(
            "pane.report_agent",
            json!({"pane_id": beta_one, "source": "test", "agent": "claude", "state": state}),
        );
    };
    report("working");
    client.wait("Build works", |client| client.row() == "○ Local  ● Build 1");
    report("blocked");
    client.wait("Build waits", |client| client.row() == "○ Local  ◉ Build 1");
    client.wait("the toast names the machine", |client| {
        client.shows("Build · beta-one needs attention")
    });
    report("idle");
    client.wait("Build waits no more", |client| {
        client.row() == "○ Local  ○ Build"
    });

    // Text typed on Local, left before it was saved, with the same notes key
    // open on Build.
    client.prefix('i');
    client.wait("Local's notes", |client| {
        client.shows("✎") && !client.shows("loading")
    });
    client.click_text("✎", 0, usize::from(COLS));
    client.wait("they are edited", |client| client.shows("esc save"));
    client.send(b"onlyalpha");
    client.wait("the text is in Local's notes", |client| {
        client.shows("onlyalpha")
    });
    client.prefix('m');
    client.wait("Build", |client| client.on("beta"));
    client.prefix('i');
    client.holds("Build has none of it", Duration::from_secs(4), |client| {
        !client.shows("onlyalpha")
    });
    assert_eq!(build.notes(), [], "nothing was written on Build");
    client.prefix('m');
    client.wait("back on Local", |client| client.on("alpha"));
    assert!(
        wait_until(READY, POLL, || local
            .notes()
            .iter()
            .any(|(_, text)| text.contains("onlyalpha"))),
        "saved on Local: {:?}",
        local.notes()
    );
    assert_eq!(build.notes(), []);
    client.prefix('i');

    // Build goes away while it is shown: its rows stay, dimmed; the client
    // stays on it, and it comes back by itself.
    client.prefix('m');
    client.wait("Build", |client| client.on("beta"));
    client.prefix('i');
    let dimmed = |client: &Client| {
        client
            .styled()
            .iter()
            .skip(1) // the header names the tab too
            .any(|line| line.contains("beta-one") && has_dim(line))
    };
    assert!(!dimmed(&client), "{:#?}", client.styled());
    build.unplug();
    assert!(build.stop(), "the server stopped");
    client.wait("Build is away", |client| client.row() == "○ Local  ↻ Build");
    client.holds("it stays selected", Duration::from_secs(3), |client| {
        client.on("beta")
    });
    assert!(dimmed(&client), "{:#?}", client.styled());
    build.start();
    build.plug_in();
    client.wait("Build is back by itself", |client| {
        client.row() == "○ Local  ○ Build" && client.on("beta")
    });
    build.print(&beta_two, "BETA_AGAIN");
    client.wait("with its pane", |client| client.shows("BETA_AGAIN"));
    assert!(!dimmed(&client), "{:#?}", client.styled());
    client.click_machine("Local");
    client.wait("Local was there all along", |client| {
        client.on("alpha") && client.shows("ALPHA_TWO")
    });
    drop(client);
    build.unplug();
    assert!(build.stop());
}
