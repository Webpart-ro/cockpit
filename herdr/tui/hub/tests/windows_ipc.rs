#![cfg(windows)]

//! Windows hub discovery, local TCP authentication, subscriptions and process lifetime in isolated homes.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROCESS: &str = "COCKPIT_TEST_WINDOWS_IPC_PROCESS";

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
    }

    fn discovery(&self) -> PathBuf {
        self.home.path().join(".cache").join("cockpit").join("hub.json")
    }

    fn command(&self, mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        let inherited = std::env::vars_os().filter(|(key, _)| {
            let key = key.to_string_lossy();
            !["HC_", "HERDR_", "COCKPIT_", "CLAUDE"].iter().any(|prefix| key.starts_with(prefix))
        });
        command
            .args(["helper_process", "--exact", "--test-threads", "1", "--nocapture"])
            .env_clear()
            .envs(inherited)
            .env(PROCESS, mode)
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("CODEX_HOME", self.home.path().join(".codex"))
            .env("CLAUDE_CONFIG_DIR", self.home.path().join(".claude"))
            .env("PI_CODING_AGENT_DIR", self.home.path().join(".pi"))
            .env("QWEN_RUNTIME_DIR", self.home.path().join(".qwen"))
            .env("COPILOT_HOME", self.home.path().join(".copilot"))
            .env("XDG_DATA_HOME", self.home.path().join(".local").join("share"))
            .env("HERDR_APP_HOME", self.home.path().join("herdr-app"))
            .env("HERDR_BIN_PATH", self.home.path().join("no-herdr.exe"))
            .env("HC_BIN", self.home.path().join("no-hc.exe"))
            .env("COCKPIT_HERDR_DIR", self.home.path().join("herdr"))
            .env("COCKPIT_FRONTEND", "tui")
            .env("COCKPIT_HUB_FEED", "off")
            .env("COCKPIT_PUBLISHED_URL", "off")
            .env("COCKPIT_EMBED", "off")
            .env("COCKPIT_HUB_IDLE_SECS", "60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        command
    }

    fn start(&self, idle: &str) -> (Process, Value) {
        let mut hub = Process(self.command("hub").env("COCKPIT_HUB_IDLE_SECS", idle).spawn().unwrap());
        let started = Instant::now();
        loop {
            if let Some(info) = cockpit_core::files::load(&self.discovery()) {
                return (hub, info);
            }
            assert!(hub.0.try_wait().unwrap().is_none(), "hub exited before publishing discovery");
            assert!(started.elapsed() < Duration::from_secs(10), "hub never published discovery");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

struct Process(Child);

impl Process {
    fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(started.elapsed() < timeout, "test process did not exit in {timeout:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn request(info: &Value, port: &str, request: Value) -> Value {
    let host = if port == "local_port" { "127.0.0.1" } else { info["host"].as_str().unwrap() };
    let mut stream = TcpStream::connect((host, info[port].as_u64().unwrap() as u16)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn helper_process() {
    let Ok(mode) = std::env::var(PROCESS) else { return };
    match mode.as_str() {
        "hub" => {
            cockpit_hub::views::register(cockpit_hub::views::Def {
                name: "acme",
                docs: &[],
                dirs: Vec::new,
                build: |_, _| json!({"frontend": cockpit_core::frontend::current().name()}),
                act: |_, _| None,
            });
            cockpit_hub::run();
        }
        "client" => {
            let call = |request: Value| cockpit_core::hub::request(&request, Duration::from_secs(3)).unwrap();
            assert_eq!(call(json!({"op": "ping"})), json!({"ok": true}));
            assert_eq!(call(json!({"op": "view", "name": "acme"}))["value"]["frontend"], "tui");
            let mut docs = cockpit_core::hub::subscribe(&["inbox"]);
            let value = json!({"tickets": [{"id": "ENG-412"}]});
            call(json!({"op": "store.put", "name": "inbox", "value": value}));
            let started = Instant::now();
            loop {
                docs.pump();
                if docs.get("inbox") == Some(&value) {
                    break;
                }
                assert!(started.elapsed() < Duration::from_secs(5), "local subscription did not receive inbox");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        "detach" => {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["helper_process", "--exact", "--test-threads", "1", "--nocapture"])
                .env(PROCESS, "sleep")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut child = cockpit_core::platform::detach(&mut command).spawn().unwrap();
            std::fs::write(cockpit_core::files::home().join("sleeper.pid"), child.id().to_string()).unwrap();
            std::thread::spawn(move || child.wait());
        }
        "sleep" => std::thread::sleep(Duration::from_secs(8)),
        "home" => assert_eq!(cockpit_core::files::home(), PathBuf::from(std::env::var_os("USERPROFILE").unwrap())),
        other => panic!("unknown helper mode {other}"),
    }
}

#[test]
fn a_detached_child_does_not_hold_its_parents_output_pipes_open() {
    struct Sleeper(u64);
    impl Drop for Sleeper {
        fn drop(&mut self) {
            cockpit_core::platform::kill(self.0);
        }
    }

    let sandbox = Sandbox::new();
    let mut parent = Process(sandbox.command("detach").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap());
    let mut stdout = parent.0.stdout.take().unwrap();
    let mut stderr = parent.0.stderr.take().unwrap();
    assert!(parent.wait(Duration::from_secs(5)).success());
    let sleeper = Sleeper(std::fs::read_to_string(sandbox.home.path().join("sleeper.pid")).unwrap().parse().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    let errors = tx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(std::io::copy(&mut stdout, &mut std::io::sink()).is_ok());
    });
    std::thread::spawn(move || {
        let _ = errors.send(std::io::copy(&mut stderr, &mut std::io::sink()).is_ok());
    });
    let started = Instant::now();
    for _ in 0..2 {
        assert!(
            rx.recv_timeout(Duration::from_secs(3).saturating_sub(started.elapsed())).expect("detached child kept its parent's output pipe open"),
            "failed reading parent's output"
        );
    }
    assert!(cockpit_core::platform::alive(sleeper.0), "EOF must arrive while the detached child still runs");
}

#[test]
fn a_second_start_keeps_the_first_hub_and_exits() {
    let sandbox = Sandbox::new();
    let (mut first, info) = sandbox.start("60");
    assert_eq!(info["pid"], first.0.id());
    let mut second = Process(sandbox.command("hub").spawn().unwrap());
    assert!(second.wait(Duration::from_secs(8)).success());
    assert!(first.0.try_wait().unwrap().is_none());
    assert_eq!(cockpit_core::files::load(&sandbox.discovery()).unwrap()["pid"], info["pid"]);
}

#[test]
fn local_clients_authenticate_ping_and_subscribe_without_forcing_the_app_frontend() {
    let sandbox = Sandbox::new();
    let (_hub, info) = sandbox.start("60");
    let token = info["token"].as_str().unwrap();
    assert_eq!(token.len(), 32);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    for port in ["port", "local_port"] {
        let denied = request(&info, port, json!({"op": "ping", "token": "wrong"}));
        assert!(denied["error"].as_str().is_some_and(|error| error.contains("token")));
        let denied = request(&info, port, json!({"op": "ping"}));
        assert!(denied["error"].is_string());
    }
    assert_eq!(request(&info, "port", json!({"op": "view", "name": "acme", "token": token}))["value"]["frontend"], "app");
    let mut client = Process(sandbox.command("client").spawn().unwrap());
    assert!(client.wait(Duration::from_secs(10)).success(), "local client helper failed");
}

#[test]
fn an_idle_hub_removes_its_discovery_and_releases_its_lock() {
    let sandbox = Sandbox::new();
    let (mut hub, _) = sandbox.start("1");
    assert!(hub.wait(Duration::from_secs(8)).success());
    assert!(!sandbox.discovery().exists());
    let (mut replacement, info) = sandbox.start("1");
    assert_eq!(info["pid"], replacement.0.id());
    assert!(replacement.wait(Duration::from_secs(8)).success());
    assert!(!sandbox.discovery().exists());
}

#[test]
fn a_force_killed_hub_leaves_nothing_that_blocks_the_next_start() {
    let sandbox = Sandbox::new();
    let (mut hub, stale) = sandbox.start("60");
    cockpit_core::platform::terminate(u64::from(hub.0.id()));
    hub.wait(Duration::from_secs(5));
    assert!(sandbox.discovery().exists(), "a killed hub cannot clean up its discovery file");
    assert!(TcpStream::connect(("127.0.0.1", stale["local_port"].as_u64().unwrap() as u16)).is_err());
    let mut next = Process(sandbox.command("hub").spawn().unwrap());
    let started = Instant::now();
    while cockpit_core::files::load(&sandbox.discovery()).is_none_or(|info| info["pid"] != next.0.id()) {
        assert!(next.0.try_wait().unwrap().is_none(), "next hub exited: the lock or discovery blocked it");
        assert!(started.elapsed() < Duration::from_secs(10), "next hub never replaced the stale discovery");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn userprofile_supplies_home_when_home_is_unset_or_empty() {
    let sandbox = Sandbox::new();
    for empty in [false, true] {
        let mut command = sandbox.command("home");
        if empty {
            command.env("HOME", "");
        } else {
            command.env_remove("HOME");
        }
        let mut child = Process(command.spawn().unwrap());
        assert!(child.wait(Duration::from_secs(5)).success());
    }
}
