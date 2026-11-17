#![cfg(windows)]

//! `cockpit hub start|ping|stop` on Windows, in an isolated home.

use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn cockpit(home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cockpit"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("COCKPIT_FRONTEND", "tui")
        .env("COCKPIT_HUB_FEED", "off")
        .env("COCKPIT_PUBLISHED_URL", "off")
        .env("COCKPIT_EMBED", "off")
        .env_remove("HC_TERMINAL_ID")
        .env_remove("HC_TERMINAL_SOCKET")
        .env_remove("HC_SCOPE")
        .output()
        .unwrap()
}

fn start(home: &std::path::Path) -> u64 {
    let out = cockpit(home, &["hub", "start"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["pid"].as_u64().unwrap()
}

#[test]
fn hub_stop_ends_the_hub_and_the_next_start_works() {
    let home = tempfile::tempdir().unwrap();
    let first = start(home.path());
    assert!(cockpit_core::platform::alive(first));
    assert!(cockpit(home.path(), &["hub", "ping"]).status.success());

    cockpit(home.path(), &["hub", "stop"]);
    let started = Instant::now();
    while cockpit_core::platform::alive(first) && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!cockpit_core::platform::alive(first), "hub {first} still runs after hub stop");

    let second = start(home.path());
    assert_ne!(first, second);
    cockpit(home.path(), &["hub", "stop"]);
}
