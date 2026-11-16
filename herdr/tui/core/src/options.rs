//! The options a person turns on or off, each listed once with the version that brought it, and what of them was shown:
//! the update dialog, `cockpit options`, setup and Diagnostics all read this list.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{files, jsonedit};

/// The version the options below arrive in: the one number to change at release.
pub const NEXT: &str = "0.41.0";

/// The layout an update offers to try, once, to a window on another layout, and the version that brought it.
pub const LAYOUT: (&str, &str) = ("cockpit-chat", NEXT);

/// How an option is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// On or off.
    Toggle,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Toggle => "toggle",
        }
    }
}

/// One option: its dotted key in config.json, the version that brought it, its name, one plain line and its default.
#[derive(Debug)]
pub struct Setting {
    pub key: &'static str,
    pub since: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub default: bool,
    pub kind: Kind,
}

/// Every option, in the order it is asked.
pub const ALL: [Setting; 3] = [
    Setting {
        key: "queue.drain",
        since: NEXT,
        title: "Send queued prompts on their own",
        description: "A prompt you queue for an agent goes by itself once the agent rests; off, you send each one.",
        default: true,
        kind: Kind::Toggle,
    },
    Setting {
        key: "ci.tellAgents",
        since: NEXT,
        title: "Tell agents when their CI fails",
        description: "A failed CI run on an agent's pull request is queued to that agent, once per commit.",
        default: true,
        kind: Kind::Toggle,
    },
    Setting {
        key: "chat.nextSteps",
        since: NEXT,
        title: "Suggest next prompts",
        description: "After each turn, a few prompts to send next, written with the agent's own model and billed to it.",
        default: false,
        kind: Kind::Toggle,
    },
];

/// Whether `a` is a later version than `b`, number by number.
pub fn is_newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parts(a), parts(b));
    (0..a.len().max(b.len())).map(|i| (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0))).find(|(x, y)| x != y).is_some_and(|(x, y)| x > y)
}

pub fn get(key: &str) -> Option<&'static Setting> {
    ALL.iter().find(|s| s.key == key)
}

/// The options a version later than `version` brought, in the list's order.
pub fn since(version: &str) -> Vec<&'static Setting> {
    ALL.iter().filter(|s| is_newer(s.since, version)).collect()
}

/// The option's value in a config: the one written at its key, else its default.
pub fn value(config: &Value, setting: &Setting) -> bool {
    setting.key.split('.').fold(config, |node, part| &node[part]).as_bool().unwrap_or(setting.default)
}

/// Where what was shown is kept: `{ran, seen, update}`.
fn state_path() -> PathBuf {
    files::cache().join("options.json")
}

/// What was shown: the version that ran last (`ran`), the options seen (`seen`), and the last update (`update`).
pub fn state() -> Value {
    files::load(&state_path()).filter(Value::is_object).unwrap_or_else(|| json!({}))
}

/// The release notes between two versions, newest first, from what the hub's check wrote in update.json.
fn notes_between(update: &Value, from: &str, to: &str) -> Value {
    let notes = update["notes"].as_array().into_iter().flatten();
    Value::Array(notes.filter(|n| n["version"].as_str().is_some_and(|v| is_newer(v, from) && !is_newer(v, to))).cloned().collect())
}

/// The state once `own` ran: a version later than the one noted last (update.json's, before anything was noted) is an
/// update, kept with its notes until it is shown.
pub fn after_run(state: &Value, update: &Value, own: &str, now: u64) -> Value {
    let mut next = state.clone();
    let last = state["ran"].as_str().or_else(|| state.get("ran").is_none().then(|| update["current"].as_str()).flatten());
    if let Some(last) = last.filter(|last| is_newer(own, last)) {
        next["update"] = json!({"from": last, "to": own, "at": now, "notes": notes_between(update, last, own), "shown": false});
    }
    next["ran"] = json!(own);
    next
}

/// Notes the version that runs, once per process start: the hub before its first look at what is published, and
/// `cockpit options`. Gives the state as it stands.
pub fn note_run(own: &str) -> Value {
    let path = state_path();
    files::locked(&path, || {
        let state = state();
        let next = after_run(&state, &files::load(&files::cache().join("update.json")).unwrap_or(Value::Null), own, crate::seen::now_ms());
        if next != state {
            let _ = files::write(&path, &next);
        }
        next
    })
}

/// The state with these options seen and, given its version, the last update shown.
pub fn marked(state: &Value, keys: &[&str], update: Option<&str>) -> Value {
    let mut next = state.clone();
    let mut seen: Vec<Value> = state["seen"].as_array().cloned().unwrap_or_default();
    for key in keys.iter().filter(|k| get(k).is_some()) {
        if !seen.iter().any(|s| s == key) {
            seen.push(json!(key));
        }
    }
    next["seen"] = Value::Array(seen);
    if let Some(to) = update
        && next["update"]["to"] == to
    {
        next["update"]["shown"] = json!(true);
    }
    next
}

/// Marks options as seen and, given its version, the last update as shown.
pub fn mark(keys: &[&str], update: Option<&str>) -> Result<(), String> {
    mark_in(&state_path(), keys, update)
}

fn mark_in(path: &Path, keys: &[&str], update: Option<&str>) -> Result<(), String> {
    files::locked(path, || {
        let state = files::load(path).filter(Value::is_object).unwrap_or_else(|| json!({}));
        files::write(path, &marked(&state, keys, update))
    })
    .map_err(|e| format!("options.json not written: {e}"))
}

/// The config's text with the option at `key` set, every other byte kept; a part of the key that holds something other
/// than an object is refused.
pub fn set_in(text: &str, key: &str, on: bool) -> Result<String, String> {
    let parts: Vec<&str> = key.split('.').collect();
    let (name, path) = parts.split_last().ok_or("an empty key")?;
    jsonedit::set(text, path, name, &json!(on)).map_err(|e| format!("{key} cannot be set in config.json ({e})"))
}

/// The value with the option at `key` set, the objects on its way made.
fn set_value(value: &mut Value, key: &str, on: bool) {
    let parts: Vec<&str> = key.split('.').collect();
    let Some((name, path)) = parts.split_last() else { return };
    let mut node = value;
    for part in path {
        if !node[*part].is_object() {
            node[*part] = json!({});
        }
        node = &mut node[*part];
    }
    node[*name] = json!(on);
}

/// Sets options in ~/.config/cockpit/config.json, everything else kept as written, and in the resolved copy the apps
/// read until a session writes it again; they count as seen.
pub fn save(values: &[(&str, bool)]) -> Result<(), String> {
    save_in(values, &files::config_dir().join("config.json"), &files::cache().join("config.json"), &state_path())
}

fn save_in(values: &[(&str, bool)], path: &Path, resolved: &Path, state: &Path) -> Result<(), String> {
    if let Some((key, _)) = values.iter().find(|(key, _)| get(key).is_none()) {
        return Err(format!("no option {key:?}"));
    }
    files::locked(path, || {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("config.json not read: {e}")),
        };
        let mut next = text.clone();
        for (key, on) in values {
            next = set_in(&next, key, *on)?;
        }
        if next != text {
            write_private(path, &next).map_err(|e| format!("config.json not written: {e}"))?;
        }
        files::locked(resolved, || {
            if let Some(mut copy) = files::load(resolved).filter(Value::is_object) {
                for (key, on) in values {
                    set_value(&mut copy, key, *on);
                }
                files::write(resolved, &copy).map_err(|e| format!("resolved config.json not written: {e}"))?;
            }
            Ok::<_, String>(())
        })?;
        mark_in(state, &values.iter().map(|(key, _)| *key).collect::<Vec<_>>(), None)
    })
}

/// The config written through a file only its owner reads, as it may hold tokens.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".config.json.{}-{n}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    file.write_all(text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// Every option with its value, default and version, whether it was seen and whether the last update brought it; the
/// last update with its notes; and the layout it offers to try (`layout`), as the views and `--json` read them.
pub fn view(config: &Value, state: &Value) -> Value {
    let seen: Vec<&str> = state["seen"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let from = state["update"]["from"].as_str();
    let options: Vec<Value> = ALL
        .iter()
        .map(|s| {
            json!({
                "key": s.key,
                "since": s.since,
                "title": s.title,
                "description": s.description,
                "default": s.default,
                "kind": s.kind.name(),
                "value": value(config, s),
                "seen": seen.contains(&s.key),
                "new": from.is_some_and(|f| is_newer(s.since, f)),
            })
        })
        .collect();
    let layout = from.filter(|f| is_newer(LAYOUT.1, f)).map(|_| LAYOUT.0);
    json!({"next": NEXT, "options": options, "update": state.get("update").cloned().unwrap_or(Value::Null), "layout": layout})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_option_is_listed_once_with_a_version_and_one_line() {
        for (i, s) in ALL.iter().enumerate() {
            assert!(ALL.iter().skip(i + 1).all(|o| o.key != s.key), "{} twice", s.key);
            assert!(!s.since.is_empty() && !s.title.is_empty() && s.description.ends_with('.'), "{}", s.key);
        }
        assert_eq!(ALL.iter().map(|s| s.key).collect::<Vec<_>>(), ["queue.drain", "ci.tellAgents", "chat.nextSteps"]);
        assert_eq!(get("chat.nextSteps").map(|s| s.default), Some(false));
        assert!(get("nope").is_none());
    }

    #[test]
    fn versions_compare_number_by_number() {
        assert!(is_newer("0.41.0", "0.40.3") && is_newer("0.40.10", "0.40.9") && is_newer("1.0", "0.99.9") && is_newer("v0.41.1", "0.41"));
        assert!(!is_newer("0.40.3", "0.40.3") && !is_newer("0.40.3", "0.41.0") && !is_newer("0.41", "0.41.0"));
        assert_eq!(since("0.40.3").len(), ALL.len());
        assert!(since(NEXT).is_empty());
    }

    #[test]
    fn a_value_is_the_one_written_else_the_default() {
        let drain = get("queue.drain").unwrap();
        assert!(value(&json!({}), drain));
        assert!(!value(&json!({"queue": {"drain": false}}), drain));
        assert!(value(&json!({"queue": "odd"}), drain), "a value that is not a boolean reads as the default");
        assert!(!value(&Value::Null, get("chat.nextSteps").unwrap()));
    }

    #[test]
    fn a_later_version_than_the_last_noted_is_an_update_with_its_notes() {
        let published = json!({"current": "0.39.0", "notes": [
            {"version": "0.41.1", "changes": ["later"]},
            {"version": "0.41.0", "changes": ["new"]},
            {"version": "0.40.0", "changes": ["older"]},
            {"version": "0.39.0", "changes": ["running"]},
        ]});
        let noted = after_run(&json!({"ran": "0.40.0", "seen": ["queue.drain"]}), &published, "0.41.0", 7);
        assert_eq!(noted["ran"], "0.41.0");
        assert_eq!(noted["seen"], json!(["queue.drain"]));
        assert_eq!(noted["update"], json!({"from": "0.40.0", "to": "0.41.0", "at": 7, "notes": [{"version": "0.41.0", "changes": ["new"]}], "shown": false}));
        // The same version again changes nothing; an older one is no update.
        assert_eq!(after_run(&noted, &published, "0.41.0", 9), noted);
        assert_eq!(after_run(&noted, &published, "0.40.0", 9)["update"], noted["update"]);
        // Before anything was noted, the version update.json was written by is the one that ran.
        let first = after_run(&json!({}), &published, "0.41.0", 7);
        assert_eq!((first["update"]["from"].as_str(), first["update"]["notes"].as_array().map(Vec::len)), (Some("0.39.0"), Some(2)));
        // A first install has nothing to say.
        assert!(after_run(&json!({}), &Value::Null, "0.41.0", 7).get("update").is_none());
    }

    #[test]
    fn marking_notes_the_options_seen_and_the_update_shown_once() {
        let state = json!({"ran": "0.41.0", "update": {"from": "0.40.0", "to": "0.41.0", "shown": false}});
        let marked = marked(&state, &["queue.drain", "made.up", "queue.drain"], Some("0.41.0"));
        assert_eq!(marked["seen"], json!(["queue.drain"]));
        assert_eq!(marked["update"]["shown"], true);
        assert_eq!(super::marked(&state, &[], Some("0.40.9"))["update"]["shown"], false, "another version's update stays to show");
    }

    #[test]
    fn setting_an_option_keeps_the_rest_of_the_file_as_written() {
        let text = "{\n  \"reposDir\": \"~/code\",\n  \"queue\": {\"drain\": true},\n  \"ci\": {\n    \"enabled\": true\n  }\n}\n";
        let done = set_in(&set_in(text, "queue.drain", false).unwrap(), "ci.tellAgents", false).unwrap();
        assert_eq!(
            done,
            "{\n  \"reposDir\": \"~/code\",\n  \"queue\": {\"drain\": false},\n  \"ci\": {\n    \"enabled\": true,\n    \"tellAgents\": false\n  }\n}\n"
        );
        assert_eq!(set_in("", "chat.nextSteps", true).unwrap(), "{\n  \"chat\": {\n    \"nextSteps\": true\n  }\n}\n");
        let refused = set_in("{\"chat\": false}", "chat.nextSteps", true).unwrap_err();
        assert!(refused.contains("chat.nextSteps cannot be set"), "{refused}");
        let mut copy = json!({"chat": false, "queue": {"drain": true}});
        set_value(&mut copy, "chat.nextSteps", true);
        set_value(&mut copy, "queue.drain", false);
        assert_eq!(copy, json!({"chat": {"nextSteps": true}, "queue": {"drain": false}}));
    }

    #[test]
    fn simultaneous_saves_keep_each_option_in_both_configs() {
        for _ in 0..12 {
            let root = tempfile::tempdir().unwrap();
            let raw = root.path().join("raw/config.json");
            let resolved = root.path().join("resolved/config.json");
            let state = root.path().join("options.json");
            let initial = json!({"queue": {"drain": true}, "ci": {"tellAgents": true}, "chat": {"nextSteps": false}, "repo": "acme/api"});
            files::write(&raw, &initial).unwrap();
            files::write(&resolved, &initial).unwrap();
            let ready = std::sync::Barrier::new(3);
            std::thread::scope(|threads| {
                for (key, on) in [("queue.drain", false), ("ci.tellAgents", false), ("chat.nextSteps", true)] {
                    let (raw, resolved, state, ready) = (&raw, &resolved, &state, &ready);
                    threads.spawn(move || {
                        ready.wait();
                        save_in(&[(key, on)], raw, resolved, state).unwrap();
                    });
                }
            });
            let expected = json!({"queue": {"drain": false}, "ci": {"tellAgents": false}, "chat": {"nextSteps": true}, "repo": "acme/api"});
            assert_eq!(files::load(&raw), Some(expected.clone()));
            assert_eq!(files::load(&resolved), Some(expected));
            assert_eq!(files::load(&state).unwrap()["seen"].as_array().unwrap().len(), 3);
        }
    }

    #[test]
    fn an_unreadable_config_is_preserved() {
        let root = tempfile::tempdir().unwrap();
        let raw = root.path().join("config.json");
        std::fs::write(&raw, [0xff]).unwrap();
        let result = save_in(&[("chat.nextSteps", true)], &raw, &root.path().join("resolved.json"), &root.path().join("options.json"));
        assert!(result.unwrap_err().contains("config.json not read"));
        assert_eq!(std::fs::read(raw).unwrap(), [0xff]);
    }

    #[test]
    fn the_view_says_what_the_last_update_brought() {
        let state = json!({"seen": ["ci.tellAgents"], "update": {"from": "0.40.3", "to": NEXT, "notes": [], "shown": false}});
        let view = view(&json!({"chat": {"nextSteps": true}}), &state);
        let options = view["options"].as_array().unwrap();
        assert_eq!(options.len(), ALL.len());
        let next = options.iter().find(|o| o["key"] == "chat.nextSteps").unwrap();
        assert_eq!(
            (next["value"].as_bool(), next["default"].as_bool(), next["new"].as_bool(), next["seen"].as_bool()),
            (Some(true), Some(false), Some(true), Some(false))
        );
        assert_eq!(options.iter().find(|o| o["key"] == "ci.tellAgents").unwrap()["seen"], true);
        assert_eq!((view["layout"].as_str(), view["update"]["to"].as_str()), (Some(LAYOUT.0), Some(NEXT)));
        let later = super::view(&json!({}), &json!({"update": {"from": NEXT, "to": "9.0.0"}}));
        assert!(later["layout"].is_null() && later["options"][0]["new"] == false, "an update past the layout's version offers nothing it had");
        assert!(super::view(&json!({}), &json!({}))["update"].is_null());
    }
}
