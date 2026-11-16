//! The Diagnostics sheet's Fix buttons: a check of the last `cockpit doctor --json` run fixed by its command, typed into
//! a tab of the actions space, or by the hub action it names.

use cockpit_core::files;
use serde_json::Value;

use super::{Ctx, arg, sources};
use crate::value::{Res, text};

/// The kept report, when this cockpit wrote it and it is the one the person looked at (`at`).
fn report(at: u64) -> Res<Value> {
    let report = files::load(&files::cache().join("doctor.json")).ok_or("no doctor report yet: run cockpit doctor --json")?;
    let ours = env!("CARGO_PKG_VERSION");
    if report["cockpit"] != ours {
        return Err(format!("the last report is from cockpit {}, this is {ours}: run the checks again", report["cockpit"].as_str().unwrap_or("?")));
    }
    if report["at"].as_u64() != Some(at) {
        return Err("the checks ran again since: look at what they say now".into());
    }
    Ok(report)
}

/// The hub actions a doctor check's fix may name: those that do what they say at once, without asking first.
const ALLOWED: [(&str, &str); 2] = [("sources", "refresh"), ("sources", "sign_in")];

/// A fix as the person saw it: its command, or its action with its arguments (`app action [args]`).
pub fn shown(fix: &Value) -> String {
    match fix["command"].as_str() {
        Some(command) => command.to_string(),
        None => format!(
            "{} {} {}",
            fix["act"]["app"].as_str().unwrap_or_default(),
            fix["act"]["action"].as_str().unwrap_or_default(),
            serde_json::to_string(fix["act"]["args"].as_array().unwrap_or(&Vec::new())).unwrap_or_default()
        ),
    }
}

/// `[check id, report at, the fix as shown]`: the fix that report gives that check, refused unless it is still the one
/// the person confirmed. Only the doctor's own commands and a few hub actions run, never one the caller sends.
pub fn fix(ctx: &mut Ctx, args: &[Value]) -> Res {
    let id = text(arg(args, 0)?);
    let at = arg(args, 1)?.as_u64().ok_or("which report: the fix names the report it was shown in")?;
    let seen = text(arg(args, 2)?);
    let report = report(at)?;
    let check =
        report["checks"].as_array().into_iter().flatten().find(|c| c["id"] == id).cloned().ok_or_else(|| format!("no check {id} in the last report"))?;
    let fix = &check["fix"];
    if shown(fix) != seen {
        return Err("The checks changed: look again".into());
    }
    let done = if let Some(act) = fix["act"].as_object() {
        let (app, action) = (act.get("app").and_then(Value::as_str).unwrap_or_default(), act.get("action").and_then(Value::as_str).unwrap_or_default());
        if !ALLOWED.contains(&(app, action)) {
            return Err(format!("{id}'s fix names {app} {action}, which a fix may not run: do it from its own place"));
        }
        let args = act.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
        let done = super::act(app, action, args, None, ctx.env.clone());
        if done["unknown"] == true {
            return Err(format!("{id}'s fix names an action this hub does not know ({app} {action})"));
        }
        if let Some(error) = done["error"].as_str() {
            return Err(error.to_string());
        }
        // An action that waits to be asked says nothing: that is no fix done.
        done["note"].as_str().filter(|n| !n.is_empty()).ok_or_else(|| format!("{id}'s fix did not run: do it from its own place"))?.to_string()
    } else {
        let Some(command) = fix["command"].as_str() else {
            return Err(format!("{} has no fix to run: {}", id, fix["label"].as_str().unwrap_or("see cockpit doctor")));
        };
        // The report says `cockpit`; the tab runs this binary, the one the hub is.
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "cockpit".into());
        let line = match command.strip_prefix("cockpit ") {
            Some(rest) => format!("{} {rest}", crate::value::quote(&exe)),
            None => command.to_string(),
        };
        let place = sources::in_tab(ctx, &format!("fix · {id}"), &line, command)?;
        match place.strip_suffix(", already open") {
            Some(open) => format!("{command} is already open {open}: finish it there"),
            None => format!("running {command} {place}"),
        }
    };
    // The next `--cached` read runs the checks again, so what the fix changed shows; a newer report stays as it is.
    let kept = files::cache().join("doctor.json");
    files::locked(&kept, || {
        if let Some(mut now) = files::load(&kept).filter(|r| r["at"].as_u64() == Some(at)) {
            now["stale"] = Value::Bool(true);
            let _ = files::write(&kept, &now);
        }
    });
    ctx.note(done);
    Ok(())
}
