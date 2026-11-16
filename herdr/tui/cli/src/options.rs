//! `cockpit options`: every option with its value and how to set it, what the last update brought, and herdr's popup
//! that asks about its new options; and the hub's `options` view, whose actions only a person's click takes.

use std::io::{IsTerminal, Write};

use cockpit_core::{files, frontend, options};
use cockpit_hub::views::Doc;
use serde_json::{Map, Value};

use crate::proc;

const USAGE: &str = "usage: cockpit options [--json | set <key> on|off | seen [<key>...] | new | ask | pending] (cockpit options --help)";

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn main(args: &[String]) {
    let state = options::note_run(VERSION);
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => print!("{}", listed(&files::config(), &state)),
        ["--json"] => println!("{}", options::view(&files::config(), &state)),
        ["set", key, word] => {
            let Some(on) = switch(word) else { proc::fail(&format!("{word:?} is not on or off\n{USAGE}")) };
            let Some(setting) = options::get(key) else { proc::fail(&format!("no option {key:?}: cockpit options lists them")) };
            options::save(&[(setting.key, on)]).unwrap_or_else(|e| proc::fail(&e));
            println!("{}: {}", setting.title, if on { "on" } else { "off" });
        }
        ["seen", keys @ ..] => {
            let keys: Vec<&str> = if keys.is_empty() { options::ALL.iter().map(|s| s.key).collect() } else { keys.to_vec() };
            options::mark(&keys, state["update"]["to"].as_str()).unwrap_or_else(|e| proc::fail(&e));
        }
        ["new"] => {
            print!("{}", news(&state, &files::config()));
            mark_shown(&state);
        }
        ["ask"] => ask(&state),
        ["pending"] => std::process::exit(if worth_showing(&state) { 0 } else { 1 }),
        _ => proc::fail(USAGE),
    }
}

/// `on`/`off` and their usual spellings.
fn switch(word: &str) -> Option<bool> {
    match word.to_lowercase().as_str() {
        "on" | "true" | "yes" | "y" | "1" => Some(true),
        "off" | "false" | "no" | "n" | "0" => Some(false),
        _ => None,
    }
}

const fn word(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// An update not shown yet that has something to say: release notes, options or a layout to try.
fn worth_showing(state: &Value) -> bool {
    let update = &state["update"];
    let from = update["from"].as_str().unwrap_or_default();
    update["shown"] == false
        && (update["notes"].as_array().is_some_and(|n| !n.is_empty()) || !options::since(from).is_empty() || offered_layout(state).is_some())
}

/// The last update shown, and the options it brought seen.
fn mark_shown(state: &Value) {
    let Some(from) = state["update"]["from"].as_str() else { return };
    let keys: Vec<&str> = options::since(from).iter().map(|s| s.key).collect();
    let _ = options::mark(&keys, state["update"]["to"].as_str());
}

/// Every option, one paragraph each, then how to set one.
fn listed(config: &Value, state: &Value) -> String {
    let view = options::view(config, state);
    let mut out = String::from("Options, in ~/.config/cockpit/config.json:\n\n");
    for o in view["options"].as_array().into_iter().flatten() {
        let on = o["value"] == true;
        let mut notes = vec![format!("default {}", word(o["default"] == true)), format!("since {}", o["since"].as_str().unwrap_or_default())];
        if o["seen"] == false {
            notes.push("new".into());
        }
        out.push_str(&format!("  {:<3}  {} ({})\n", word(on), o["title"].as_str().unwrap_or_default(), notes.join(", ")));
        out.push_str(&format!("       {}\n", o["description"].as_str().unwrap_or_default()));
        out.push_str(&format!("       cockpit options set {} {}\n\n", o["key"].as_str().unwrap_or_default(), word(!on)));
    }
    out
}

/// What the last update brought: its release notes, then each new option with its value and how to set it.
fn news(state: &Value, config: &Value) -> String {
    let update = &state["update"];
    let (Some(from), Some(to)) = (update["from"].as_str(), update["to"].as_str()) else {
        return "No update since cockpit last ran here. Every option: cockpit options\n".into();
    };
    let mut out = format!("Cockpit {to} is installed (you ran {from}).\n");
    let notes = update["notes"].as_array().cloned().unwrap_or_default();
    if !notes.is_empty() {
        out.push_str("\nWhat's new\n");
        for note in &notes {
            let date = note["date"].as_str().filter(|d| !d.is_empty()).map(|d| format!(" ({d})")).unwrap_or_default();
            out.push_str(&format!("  {}{date}\n", note["version"].as_str().unwrap_or_default()));
            for change in note["changes"].as_array().into_iter().flatten() {
                out.push_str(&format!("  - {}\n", change.as_str().unwrap_or_default()));
            }
        }
    }
    let new = options::since(from);
    if !new.is_empty() {
        out.push_str("\nNew options\n");
        for s in new {
            let on = options::value(config, s);
            out.push_str(&format!("  {}: {}{}\n", s.title, word(on), if on == s.default { " (the default)" } else { "" }));
            out.push_str(&format!("    {}\n", s.description));
            out.push_str(&format!("    cockpit options set {} {}\n", s.key, word(!on)));
        }
    }
    if let Some(layout) = offered_layout(state) {
        out.push_str(&format!("\nTry Cockpit Chat\n  In Herdr desktop: herdr-app do layout.switch '{{\"name\":\"{layout}\"}}'\n  Switch back with ⌘⇧L.\n"));
    }
    out.push_str("\nEvery option: cockpit options\n");
    out
}

/// The desktop layout this update offers to try.
fn offered_layout(state: &Value) -> Option<&'static str> {
    state["update"]["from"].as_str().filter(|from| options::is_newer(options::LAYOUT.1, from)).map(|_| options::LAYOUT.0)
}

/// Whether the person chooses the offered layout; Enter and the end of input keep the current one.
fn ask_layout(state: &Value, lines: &mut impl Iterator<Item = String>, out: &mut dyn Write) -> bool {
    if offered_layout(state).is_none() {
        return false;
    }
    let _ = write!(out, "\nTry Cockpit Chat in the Herdr desktop app? (y/n) [n] ");
    let _ = out.flush();
    lines.next().is_some_and(|line| switch(line.trim()) == Some(true))
}

/// herdr's popup after an update: what it brought, then each new option asked (Enter keeps its value), saved at once.
fn ask(state: &Value) {
    let config = files::config();
    let mut lines = std::io::stdin().lines().map_while(Result::ok);
    let (said, picked) = asked(state, &config, &mut lines, &mut std::io::stdout());
    print!("{said}");
    let changed: Vec<(&str, bool)> = picked.iter().filter(|(s, on)| options::value(&config, s) != *on).map(|(s, on)| (s.key, *on)).collect();
    if !changed.is_empty() {
        options::save(&changed).unwrap_or_else(|e| proc::fail(&e));
    }
    if ask_layout(state, &mut lines, &mut std::io::stdout()) {
        let name = offered_layout(state).unwrap();
        if frontend::app_home().join("layouts").join(format!("{name}.json")).is_file()
            && frontend::app_do("layout.switch", &serde_json::json!({"name": name})).is_some()
        {
            println!("\nOpened Cockpit Chat. Switch back with ⌘⇧L.");
        } else {
            println!("\nOpen Herdr desktop and pick Cockpit Chat from its layout menu. Switch back with ⌘⇧L.");
        }
    }
    mark_shown(state);
    if std::io::stdin().is_terminal() {
        print!("Press Enter to close. ");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().lines().next();
    }
}

/// The popup's words up to its questions, written as they are asked, and each new option's answer; then its closing words.
fn asked(state: &Value, config: &Value, mut lines: impl Iterator<Item = String>, out: &mut dyn Write) -> (String, Vec<(&'static options::Setting, bool)>) {
    let from = state["update"]["from"].as_str().unwrap_or_default();
    let news = news(state, config);
    let new = if from.is_empty() { Vec::new() } else { options::since(from) };
    // The news without its option lines: each option is asked in its place.
    let head = news
        .split("\nNew options\n")
        .next()
        .unwrap_or_default()
        .split("\nTry Cockpit Chat\n")
        .next()
        .unwrap_or_default()
        .trim_end_matches("\nEvery option: cockpit options\n");
    let _ = write!(out, "{head}");
    let mut picked = Vec::new();
    if !new.is_empty() {
        let _ = writeln!(out, "\nNew options: y or n, Enter keeps the value in brackets.");
    }
    for s in new {
        let on = options::value(config, s);
        let _ = write!(out, "\n{}\n  {}\n  Turn it on? (y/n) [{}] ", s.title, s.description, if on { "y" } else { "n" });
        let _ = out.flush();
        // The terminal's echo of Enter ends the line.
        let line = lines.next().unwrap_or_default();
        picked.push((s, switch(line.trim()).unwrap_or(on)));
    }
    let mut said = String::new();
    if !picked.is_empty() {
        let set: Vec<String> = picked.iter().map(|(s, on)| format!("{} {}", s.key, word(*on))).collect();
        said.push_str(&format!("\nNow: {}.\nChange one any time: cockpit options set <key> on|off\n", set.join(", ")));
    }
    (said, picked)
}

// ---------------------------------------------------------------------------------------------------------------------
// The hub's view

/// `view options`: every option, the last update with its notes and the layout it offers (core's `options::view`).
pub fn view(_args: &Value, _doc: Doc) -> Value {
    options::view(&files::config(), &options::state())
}

/// `set {key: bool…}` saves options, `shown {keys, update}` notes what the person was shown: a person's alone.
pub fn act(action: &str, args: &[Value]) -> Option<Result<String, String>> {
    if !["set", "shown"].contains(&action) {
        return None;
    }
    if let Err(why) = cockpit_core::peer::person(&format!("options {action}: options are set by a person, not by an agent's tools")) {
        return Some(Err(why));
    }
    let arg = args.first().cloned().unwrap_or(Value::Null);
    Some(match action {
        "set" => set(arg.as_object().cloned().unwrap_or_default()),
        _ => {
            let keys: Vec<&str> = arg["keys"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            options::mark(&keys, arg["update"].as_str()).map(|()| "noted".into())
        }
    })
}

fn set(values: Map<String, Value>) -> Result<String, String> {
    let picked: Vec<(&str, bool)> =
        values.iter().map(|(k, v)| v.as_bool().map(|on| (k.as_str(), on)).ok_or_else(|| format!("{k} takes true or false"))).collect::<Result<_, _>>()?;
    if picked.is_empty() {
        return Ok("nothing to save".into());
    }
    options::save(&picked)?;
    Ok(picked.iter().map(|(k, on)| format!("{k} {}", word(*on))).collect::<Vec<_>>().join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn updated() -> Value {
        json!({"ran": "0.41.0", "seen": [], "update": {"from": "0.40.3", "to": "0.41.0", "shown": false,
            "notes": [{"version": "0.41.0", "date": "2026-11-20", "changes": ["Quotas grouped by provider"]}]}})
    }

    #[test]
    fn the_news_names_each_new_option_with_how_to_set_it() {
        let said = news(&updated(), &json!({"queue": {"drain": false}}));
        assert!(
            said.starts_with("Cockpit 0.41.0 is installed (you ran 0.40.3).\n\nWhat's new\n  0.41.0 (2026-11-20)\n  - Quotas grouped by provider\n"),
            "{said}"
        );
        assert!(said.contains("  Send queued prompts on their own: off\n"), "{said}");
        assert!(said.contains("    cockpit options set queue.drain on\n"), "{said}");
        assert!(said.contains("  Suggest next prompts: off (the default)\n    After each turn"), "{said}");
        assert!(said.contains("Try Cockpit Chat\n  In Herdr desktop: herdr-app do layout.switch '{\"name\":\"cockpit-chat\"}'"), "{said}");
        assert!(said.ends_with("\nEvery option: cockpit options\n"));
        assert!(news(&json!({}), &json!({})).starts_with("No update since"));
    }

    #[test]
    fn the_popup_asks_each_new_option_and_enter_keeps_its_value() {
        let mut out = Vec::new();
        let answers = ["n", "maybe", "y"].map(String::from);
        let (said, picked) = asked(&updated(), &json!({}), answers.into_iter(), &mut out);
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.starts_with("Cockpit 0.41.0 is installed") && !shown.contains("cockpit options set"), "{shown}");
        assert_eq!(shown.matches("Turn it on? (y/n)").count(), 3);
        assert!(shown.contains("Suggest next prompts\n  After each turn") && shown.contains("Turn it on? (y/n) [n] "), "{shown}");
        let got: Vec<(&str, bool)> = picked.iter().map(|(s, on)| (s.key, *on)).collect();
        assert_eq!(got, [("queue.drain", false), ("ci.tellAgents", true), ("chat.nextSteps", true)], "an answer that is no y or n keeps the value");
        assert!(said.contains("Now: queue.drain off, ci.tellAgents on, chat.nextSteps on."), "{said}");
        // The input ending keeps every value.
        let (_, kept) = asked(&updated(), &json!({"chat": {"nextSteps": true}}), std::iter::empty(), &mut Vec::new());
        assert!(kept.iter().all(|(s, on)| *on == (s.default || s.key == "chat.nextSteps")));
    }

    #[test]
    fn trying_the_new_layout_requires_an_explicit_yes() {
        for (answer, expected) in [("y", true), ("yes", true), ("", false), ("n", false), ("maybe", false)] {
            let mut out = Vec::new();
            assert_eq!(ask_layout(&updated(), &mut [answer.to_string()].into_iter(), &mut out), expected);
            assert!(String::from_utf8(out).unwrap().contains("Try Cockpit Chat in the Herdr desktop app? (y/n) [n]"));
        }
        assert!(!ask_layout(&updated(), &mut std::iter::empty(), &mut Vec::new()));
        let mut out = Vec::new();
        assert!(!ask_layout(&json!({"update": {"from": options::LAYOUT.1}}), &mut ["y".into()].into_iter(), &mut out));
        assert!(out.is_empty(), "a layout the previous version already had is not offered");
    }

    #[test]
    fn an_update_is_worth_showing_while_it_has_something_to_say_and_was_not_shown() {
        assert!(worth_showing(&updated()));
        let mut shown = updated();
        shown["update"]["shown"] = json!(true);
        assert!(!worth_showing(&shown));
        assert!(!worth_showing(&json!({"update": {"from": "0.41.0", "to": "0.41.1", "notes": [], "shown": false}})));
        assert!(!worth_showing(&json!({})));
    }

    #[test]
    fn the_list_says_each_value_default_and_how_to_flip_it() {
        let said = listed(&json!({"chat": {"nextSteps": true}}), &json!({"seen": ["queue.drain"]}));
        assert!(said.contains("  on   Send queued prompts on their own (default on, since 0.41.0)\n"), "{said}");
        assert!(said.contains("  on   Suggest next prompts (default off, since 0.41.0, new)\n"), "{said}");
        assert!(said.contains("       cockpit options set chat.nextSteps off\n"), "{said}");
    }

    #[test]
    fn the_view_s_actions_refuse_what_is_not_a_person_s() {
        // A test thread serves no connection and says nothing: what only a person does fails closed there.
        assert!(act("set", &[json!({"queue.drain": false})]).unwrap().is_err());
        assert!(act("shown", &[json!({"keys": ["queue.drain"]})]).unwrap().is_err());
        assert!(act("other", &[]).is_none());
        assert_eq!(set(Map::new()).unwrap(), "nothing to save");
        assert!(set(json!({"queue.drain": "off"}).as_object().cloned().unwrap()).unwrap_err().contains("true or false"));
    }
}
