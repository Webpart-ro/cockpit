//! My mail through the hub's own mail servers (the config's `mail` section, one server per account): the store's
//! `mail` document, the new unread messages worth a notification, and the threads read whole, kept a week in
//! ~/.cache/cockpit/mail/.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use cockpit_connectors::mail;
use cockpit_connectors::tools::{Mapped, Tools};
use serde_json::{Value, json};

use crate::value::list;

/// A thread read whole is kept this long.
const KEEP_THREADS: Duration = Duration::from_secs(7 * 86_400);

/// The ids `notified` keeps, so a message that leaves the list and comes back is not announced again.
const KEEP_NOTIFIED: usize = 300;

/// How many threads one round reads ahead, so a full inbox fills over a few rounds without holding the feed up.
const READ_AHEAD: usize = 10;

/// One account's inbox as a round fetched it: the account (`{id, name}`) and its messages, each tagged with the
/// account's id, or why they could not be read.
pub type Fetched = (Value, Result<Vec<Value>, String>);

/// The inbox of each mail account the hub reaches a server for, and the threads of the listed messages read ahead over
/// the same connection, so opening one is instant; None when the hub serves no account.
pub fn fetch(raw: &Value, config: &Value) -> Option<Vec<Fetched>> {
    let served: Vec<_> =
        mail::accounts(&raw["mail"]).into_iter().filter_map(|account| Some((crate::feed::mcp::mail_server(raw, config, &account)?, account))).collect();
    if served.is_empty() {
        return None;
    }
    Some(
        served
            .into_iter()
            .map(|(server, account)| {
                let fetched = fetch_account(&server, &account);
                (json!({"id": account["id"], "name": account["name"]}), fetched)
            })
            .collect(),
    )
}

fn fetch_account(server: &cockpit_connectors::Server, account: &Value) -> Result<Vec<Value>, String> {
    let mapping = mail::mapping(account);
    let id = account["id"].as_str().unwrap_or(mail::DEFAULT_ACCOUNT).to_string();
    crate::feed::mcp::with(server, async |c| {
        let tools = Mapped { connection: c, section: &mapping };
        let mut messages = mail::inbox(&tools, &mapping).await?;
        if id != mail::DEFAULT_ACCOUNT {
            for message in &mut messages {
                message["account"] = id.clone().into();
            }
        }
        let ahead = if tools.has("get") { READ_AHEAD } else { 0 };
        for message in messages.iter().filter(|m| !kept_fresh(m)).take(ahead) {
            if message["id"].as_str().is_some_and(|id| !id.is_empty()) {
                keep_thread(&mail::key_of(message), mail::thread(&tools, &mapping, message).await);
            }
        }
        Ok(messages)
    })
}

/// Whether the message's thread is kept, read since the message's date: a reply landing makes it stale.
pub fn kept_fresh(message: &Value) -> bool {
    let Some(doc) = cockpit_core::files::load(&thread_path(&mail::key_of(message))) else { return false };
    let date = message["date"].as_str().and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok()).map_or(0, |d| d.timestamp_millis());
    doc["error"].is_null() && doc["at"].as_i64().unwrap_or(0) >= date
}

/// The `mail` document after a round: each fetched account's messages in place of its old ones, the others' kept
/// (an account that failed, or one a Claude Code session reads), every unread one's key in `notified`, and `accounts`
/// saying when each was read and why one failed. `configured` is the config's accounts: messages of any other go.
pub fn merge(doc: &Value, fetched: Vec<Fetched>, configured: &[Value], now: u64) -> Value {
    let mut doc = if doc.is_object() { doc.clone() } else { json!({"at": 0, "messages": [], "notified": []}) };
    let ids: HashSet<&str> = configured.iter().filter_map(|a| a["id"].as_str()).collect();
    let fresh: HashSet<String> = fetched.iter().filter(|(_, f)| f.is_ok()).filter_map(|(a, _)| a["id"].as_str().map(str::to_string)).collect();
    let mut messages: Vec<Value> =
        list(&doc["messages"]).iter().filter(|m| ids.contains(mail::account_of(m)) && !fresh.contains(mail::account_of(m))).cloned().collect();
    let mut states: Vec<Value> = list(&doc["accounts"]).iter().filter(|a| a["id"].as_str().is_some_and(|id| ids.contains(id))).cloned().collect();
    let mut errors = Vec::new();
    for (account, result) in fetched {
        let mut state = json!({"id": account["id"], "name": account["name"], "by": "hub"});
        match result {
            Ok(listed) => {
                state["at"] = now.into();
                messages.extend(listed);
            }
            Err(error) => {
                state["at"] = states.iter().find(|s| s["id"] == account["id"]).map_or(json!(0), |s| s["at"].clone());
                state["error"] = error.clone().into();
                errors.push(if configured.len() > 1 { format!("{}: {error}", account["name"].as_str().unwrap_or_default()) } else { error });
            }
        }
        states.retain(|s| s["id"] != account["id"]);
        states.push(state);
    }
    let order = |id: &Value| configured.iter().position(|a| a["id"] == *id).unwrap_or(usize::MAX);
    states.sort_by_key(|s| order(&s["id"]));
    messages.sort_by(|a, b| b["date"].as_str().unwrap_or_default().cmp(a["date"].as_str().unwrap_or_default()));
    let unread: Vec<Value> = messages.iter().filter(|m| m["unread"] == true).map(|m| mail::key_of(m).into()).collect();
    let known: HashSet<&Value> = unread.iter().collect();
    let kept = list(&doc["notified"]).iter().filter(|id| !known.contains(id)).cloned();
    let notified: Vec<Value> = unread.iter().cloned().chain(kept).take(KEEP_NOTIFIED).collect();
    if !fresh.is_empty() {
        doc["at"] = now.into();
    }
    doc["by"] = "hub".into();
    doc["messages"] = messages.into();
    doc["notified"] = notified.into();
    doc["accounts"] = states.into();
    if errors.is_empty() {
        doc.as_object_mut().map(|d| d.remove("error"));
    } else {
        doc["error"] = errors.join(" · ").into();
    }
    doc
}

/// The unread messages new since the last fetch, as notifications (title, body, click), each naming its account when
/// there are several; none on the first fetch, and one summing them up past three.
pub fn news(before: &Value, after: &Value) -> Vec<(String, String, String)> {
    if !before.is_object() || before["at"].as_u64().unwrap_or(0) == 0 {
        return Vec::new();
    }
    let known: HashSet<&Value> = list(&before["notified"]).iter().collect();
    // Only mail newer than any listed before in its account: a longer listing brings older unread mail in, which is not news.
    let stamp = |m: &Value| m["date"].as_str().and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok()).map_or(0, |d| d.timestamp_millis());
    let newest = |account: &str| list(&before["messages"]).iter().filter(|m| mail::account_of(m) == account).map(stamp).max().unwrap_or(0);
    // An account read for the first time announces nothing, as the first fetch does not.
    let seen = |account: &str| match list(&before["accounts"]).iter().find(|a| a["id"] == account) {
        Some(state) => state["at"].as_u64().unwrap_or(0) > 0,
        None => account == mail::DEFAULT_ACCOUNT && list(&before["accounts"]).is_empty(),
    };
    let failed = |account: &str| list(&after["accounts"]).iter().any(|a| a["id"] == account && a["error"].is_string());
    let new: Vec<&Value> = list(&after["messages"])
        .iter()
        .filter(|m| m["unread"] == true && !known.contains(&Value::from(mail::key_of(m))))
        .filter(|m| seen(mail::account_of(m)) && !failed(mail::account_of(m)) && stamp(m) > newest(mail::account_of(m)))
        .collect();
    let accounts = list(&after["accounts"]);
    let named = |m: &Value| {
        let name = accounts.iter().find(|a| a["id"] == mail::account_of(m)).and_then(|a| a["name"].as_str()).filter(|_| accounts.len() > 1);
        name.map(|n| format!(" · {n}")).unwrap_or_default()
    };
    let text = |m: &Value, key: &str| m[key].as_str().unwrap_or_default().to_string();
    if new.len() > 3 {
        let lines: Vec<String> = new.iter().take(3).map(|m| format!("{}{}: {}", text(m, "fromName"), named(m), text(m, "subject"))).collect();
        return vec![(format!("✉️ {} new messages", new.len()), format!("{}\n…", lines.join("\n")), "space:mail".into())];
    }
    new.iter()
        .map(|m| {
            let subject = text(m, "subject");
            let body = if subject.is_empty() { text(m, "snippet") } else { subject };
            (format!("✉️ {}{}", text(m, "fromName"), named(m)), body, format!("mail:{}", mail::key_of(m)))
        })
        .collect()
}

/// The folder of the threads read whole.
pub fn threads_dir() -> PathBuf {
    cockpit_core::files::cache().join("mail")
}

/// Where a message's thread is kept, by the message's key (see `mail::key`), as `{at, id, messages, error?}`.
pub fn thread_path(key: &str) -> PathBuf {
    threads_dir().join(format!("{}.json", mail::cache_name(key)))
}

/// Keeps a thread read whole, by its message's key, or why it could not be read.
pub fn keep_thread(key: &str, read: Result<Vec<Value>, String>) {
    let mut doc = json!({"at": super::now_ms(), "id": key, "messages": []});
    match read {
        Ok(messages) => doc["messages"] = Value::from(messages),
        Err(error) => doc["error"] = Value::from(error.chars().take(300).collect::<String>()),
    }
    let _ = cockpit_core::files::write(&thread_path(key), &doc);
}

/// The message of that account as the `mail` document lists it, else its id alone (its thread then taken for its own).
pub fn listed(account: &str, id: &str) -> Value {
    let doc = crate::store::global().get("mail");
    let key = mail::key(account, id);
    list(&doc["messages"]).iter().find(|m| mail::key_of(m) == key).cloned().unwrap_or_else(|| {
        let mut message = json!({"id": id, "threadId": id});
        if !account.is_empty() && account != mail::DEFAULT_ACCOUNT {
            message["account"] = account.into();
        }
        message
    })
}

/// Drops the threads, pages and marks older than a week.
pub fn prune_threads() {
    for entry in std::fs::read_dir(threads_dir()).into_iter().flatten().flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > KEEP_THREADS);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message dated by its id's letter: a at 10:01, b at 10:02…
    fn message(id: &str, unread: bool) -> Value {
        let minute = id.bytes().next().map_or(0, |b| b - b'a' + 1);
        json!({"id": id, "fromName": "Ana", "subject": format!("About {id}"), "url": format!("https://mail/{id}"), "unread": unread,
               "date": format!("2026-11-04T10:{minute:02}:00Z")})
    }

    /// The same message in another account.
    fn of(account: &str, mut message: Value) -> Value {
        message["account"] = account.into();
        message
    }

    fn default() -> Vec<Value> {
        mail::accounts(&json!({"name": "Mail"}))
    }

    /// One round of the implied account.
    fn one(fetched: Result<Vec<Value>, String>) -> Vec<Fetched> {
        vec![(json!({"id": "default", "name": "Mail"}), fetched)]
    }

    #[test]
    fn a_fetch_keeps_the_unread_ids_and_a_failure_keeps_the_messages() {
        let before = json!({"at": 5, "by": "hub", "messages": [message("a", true)], "notified": ["a", "gone"], "error": "Mail: old"});
        let after = merge(&before, one(Ok(vec![message("b", true), message("c", false)])), &default(), 9);
        assert_eq!(
            after,
            json!({"at": 9, "by": "hub", "messages": [message("c", false), message("b", true)], "notified": ["b", "a", "gone"],
                   "accounts": [{"id": "default", "name": "Mail", "by": "hub", "at": 9}]})
        );
        let failed = merge(&after, one(Err("Mail: HTTP 502".into())), &default(), 12);
        assert_eq!((&failed["messages"], &failed["at"], &failed["error"]), (&after["messages"], &json!(9), &json!("Mail: HTTP 502")));
        assert_eq!(failed["accounts"][0], json!({"id": "default", "name": "Mail", "by": "hub", "at": 9, "error": "Mail: HTTP 502"}));
        assert_eq!(merge(&Value::Null, one(Err("x".into())), &default(), 1)["messages"], json!([]));
    }

    #[test]
    fn each_account_replaces_only_its_own_messages() {
        let configured =
            mail::accounts(&json!({"accounts": [{"id": "work", "name": "Acme"}, {"id": "home", "name": "Home"}, {"id": "gmail", "name": "Gmail"}]}));
        let before = json!({"at": 5, "messages": [of("work", message("a", true)), of("home", message("a", true)), of("gmail", message("b", true)), message("z", true)],
                            "notified": [], "accounts": [{"id": "gmail", "name": "Gmail", "by": "session", "at": 4}]});
        let fetched = vec![
            (json!({"id": "work", "name": "Acme"}), Ok(vec![of("work", message("c", true))])),
            (json!({"id": "home", "name": "Home"}), Err("Mail: timed out".to_string())),
        ];
        let after = merge(&before, fetched, &configured, 9);
        let keys: Vec<String> = list(&after["messages"]).iter().map(mail::key_of).collect();
        assert_eq!(keys, ["work:c", "gmail:b", "home:a"], "work's new list, home's kept on failure, the session's untouched, an unknown account's gone");
        assert_eq!(after["notified"], json!(["work:c", "gmail:b", "home:a"]));
        let states: Vec<(&str, &str)> = list(&after["accounts"]).iter().map(|a| (a["id"].as_str().unwrap(), a["by"].as_str().unwrap())).collect();
        assert_eq!(states, [("work", "hub"), ("home", "hub"), ("gmail", "session")], "in the config's order");
        assert_eq!((&after["accounts"][1]["error"], &after["error"]), (&json!("Mail: timed out"), &json!("Home: Mail: timed out")));
    }

    #[test]
    fn news_is_the_unread_messages_not_announced_yet() {
        let first = merge(&Value::Null, one(Ok(vec![message("a", true)])), &default(), 1);
        assert_eq!(news(&Value::Null, &first), [], "the first fetch announces nothing");
        let second = merge(&first, one(Ok(vec![message("b", true), message("c", false), message("a", true)])), &default(), 2);
        assert_eq!(news(&first, &second), [("✉️ Ana".to_string(), "About b".to_string(), "mail:b".to_string())]);
        let many: Vec<Value> = ["d", "e", "f", "g"].iter().map(|id| message(id, true)).collect();
        let third = merge(&second, one(Ok(many)), &default(), 3);
        let told = news(&second, &third);
        assert_eq!((told.len(), told[0].0.as_str(), told[0].2.as_str()), (1, "✉️ 4 new messages", "space:mail"));
        assert!(told[0].1.starts_with("Ana: About g\nAna: About f"));
        assert_eq!(news(&third, &merge(&third, one(Err("down".into())), &default(), 4)), []);
        let older = merge(&third, one(Ok(vec![message("c", true), message("g", true)])), &default(), 5);
        assert_eq!(news(&third, &older), [], "an older unread message the listing brings in is no news");
        let zero = merge(&third, one(Ok(vec![])), &default(), 6);
        let back = merge(&zero, one(Ok(vec![message("h", true)])), &default(), 7);
        assert_eq!(news(&zero, &back).len(), 1, "after inbox zero, new mail is news");
    }

    #[test]
    fn news_names_the_account_and_a_new_account_announces_nothing() {
        let configured = mail::accounts(&json!({"accounts": [{"id": "work", "name": "Acme"}, {"id": "home", "name": "Home"}]}));
        let round =
            |work: Vec<Value>, home: Vec<Value>| vec![(json!({"id": "work", "name": "Acme"}), Ok(work)), (json!({"id": "home", "name": "Home"}), Ok(home))];
        let first = merge(&json!({"at": 1, "messages": [], "notified": []}), round(vec![of("work", message("a", true))], vec![]), &configured, 2);
        assert_eq!(news(&json!({"at": 1}), &first), [], "an account read for the first time");
        let second =
            merge(&first, round(vec![of("work", message("b", true)), of("work", message("a", true))], vec![of("home", message("b", true))]), &configured, 3);
        let told = news(&first, &second);
        assert_eq!(
            told,
            [
                ("✉️ Ana · Acme".to_string(), "About b".to_string(), "mail:work:b".to_string()),
                ("✉️ Ana · Home".to_string(), "About b".to_string(), "mail:home:b".to_string()),
            ],
            "the same id in two accounts is two messages"
        );
    }
}
