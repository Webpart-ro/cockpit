//! The models each agent lists for itself (`Agent::model_listing`), asked in the background at most once a day and kept
//! in `~/.cache/cockpit/models.json`, so a form offers them without waiting on the agent.

use std::collections::HashSet;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::{self, Agent};
use crate::files;

/// How long a list is kept before the agent is asked again, in ms.
const KEPT: u64 = 24 * 3_600_000;
/// The most an agent's listing may take.
const LIMIT: Duration = Duration::from_secs(30);

static ASKING: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static WRITING: Mutex<()> = Mutex::new(());

fn path() -> PathBuf {
    files::cache().join("models.json")
}

/// The models the agent listed when last asked, asking it again in the background when that was over a day ago (and it
/// is installed); empty until it first answered, and for an agent that lists none.
pub fn of(agent: &Agent, now: u64) -> Vec<String> {
    of_in(&path(), agent, now)
}

fn of_in(file: &Path, agent: &Agent, now: u64) -> Vec<String> {
    let Some((argv, how)) = agent.model_listing() else { return Vec::new() };
    let kept = files::load(file).unwrap_or(Value::Null);
    let entry = &kept[&agent.id];
    let list: Vec<String> = entry["models"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_string)).collect();
    let fresh = entry["attempted"].as_u64().or_else(|| entry["at"].as_u64()).is_some_and(|at| now.saturating_sub(at) < KEPT);
    if !fresh && agent.installed() && claim(&agent.id) {
        let (id, file) = (agent.id.clone(), file.to_path_buf());
        std::thread::spawn(move || {
            let models = run(&argv).map(|out| agent::listed_models(&out, how));
            keep(&file, &id, models.as_deref(), now);
            ASKING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).remove(&id);
        });
    }
    list
}

/// Whether this process may ask the agent now: none of its threads asks it already.
fn claim(id: &str) -> bool {
    ASKING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(id.to_string())
}

fn keep(file: &Path, id: &str, models: Option<&[String]>, now: u64) {
    let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut kept = files::load(file).filter(Value::is_object).unwrap_or_else(|| json!({}));
    if let Some(models) = models {
        kept[id] = json!({"at": now, "models": models});
    }
    if !kept[id].is_object() {
        kept[id] = json!({});
    }
    kept[id]["attempted"] = json!(now);
    kept[id]["failed"] = json!(models.is_none());
    let _ = files::write(file, &kept);
}

/// The listing's output, None when it failed or ran past its limit.
fn run(argv: &[String]) -> Option<String> {
    let mut cmd = Command::new(argv.first()?);
    cmd.args(&argv[1..]);
    let out = output_within(&mut cmd, LIMIT)?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A command's output once it ends, its input empty and Herdr's variables left out (it is no agent of a terminal);
/// None when it could not start, or ran past `limit` and was stopped.
pub fn output_within(cmd: &mut Command, limit: Duration) -> Option<std::process::Output> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    for (k, _) in std::env::vars() {
        if k.starts_with("HERDR_") || k.starts_with("HC_") {
            cmd.env_remove(k);
        }
    }
    let child = cmd.spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(out) => out.ok(),
        Err(_) => {
            let _ = Command::new("/bin/kill").args(["-KILL", &format!("-{pid}")]).status();
            let _ = rx.recv_timeout(Duration::from_secs(1));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn wait_for(file: &Path, id: &str) -> Value {
        for _ in 0..100 {
            if let Some(v) = files::load(file).filter(|v| v[id].is_object()) {
                return v;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{id} was never listed");
    }

    #[test]
    fn an_agent_s_models_are_asked_once_a_day_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("opencode");
        std::fs::write(&bin, "#!/bin/sh\n[ \"$1\" = models ] && printf 'acme/large\\nacme/small\\n\\nacme/large\\n'\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut agent = agent::builtin().into_iter().find(|a| a.id == "opencode").unwrap();
        agent.bin = bin.to_string_lossy().into_owned();
        let file = dir.path().join("models.json");
        assert_eq!(of_in(&file, &agent, 1_000), Vec::<String>::new(), "nothing until it answered");
        assert_eq!(wait_for(&file, "opencode")["opencode"]["models"], json!(["acme/large", "acme/small"]));
        assert_eq!(of_in(&file, &agent, 2_000), ["acme/large", "acme/small"]);
        std::fs::write(&bin, "#!/bin/sh\nprintf 'acme/new\\n'\n").unwrap();
        assert_eq!(of_in(&file, &agent, 3_000), ["acme/large", "acme/small"], "kept for a day");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(files::load(&file).unwrap()["opencode"]["at"], 1_000, "not asked again within the day");
        let codex = agent::builtin().into_iter().find(|a| a.id == "codex").unwrap();
        assert!(of_in(&file, &codex, 3_000).is_empty(), "an agent with no listing lists none");
    }

    #[test]
    fn each_listing_reads_its_own_way() {
        use agent::{Listing, listed_models};
        let table = "provider   model             context  max-out\nanthropic  claude-sonnet-5  1M       64K\nacme       large             200K     8K\n";
        assert_eq!(listed_models(table, Listing::Table), ["anthropic/claude-sonnet-5", "acme/large"]);
        let lines = "gemini-3.8-flash-medium        Gemini 3.8 Flash (Medium)\nacme-pro  Acme Pro\n";
        assert_eq!(listed_models(lines, Listing::Lines), ["gemini-3.8-flash-medium", "acme-pro"]);
        let json = r#"{"models": [{"provider": "acme", "id": "large", "selector": "acme/large"}, {"selector": "acme/large"}, {"id": "x"}]}"#;
        assert_eq!(listed_models(json, Listing::Json), ["acme/large"]);
        assert!(listed_models("Error: not signed in; run `acme login`", Listing::Table).is_empty(), "no header and row");
        assert!(listed_models("$(touch x)\n", Listing::Lines).is_empty(), "nothing a shell would act on");
    }

    #[test]
    fn a_failed_refresh_preserves_the_last_good_models() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("models.json");
        keep(&file, "opencode", Some(&["acme/large".into()]), 1);
        keep(&file, "opencode", None, 2);
        let kept = files::load(&file).unwrap();
        assert_eq!(kept["opencode"], json!({"models": ["acme/large"], "at": 1, "attempted": 2, "failed": true}));
    }

    #[test]
    fn a_listing_timeout_stops_a_child_that_holds_the_output_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("escaped");
        let mut command = Command::new("sh");
        command.args(["-c", "(sleep 1; touch \"$1\") & wait", "sh"]).arg(&marker);
        assert!(output_within(&mut command, Duration::from_millis(50)).is_none());
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists(), "a descendant must not survive the listing timeout");
    }
}
