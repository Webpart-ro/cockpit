//! Words never go into a menu cockpit could not read: a demo agent whose menu has scrolled past its first choice.

use cockpit_agents::gui;
use serde_json::{Value, json};

#[test]
fn words_for_a_menu_cockpit_cannot_read_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let world = home.path().join("world.json");
    let screen = "⏺ Running the export again.\n\n❯ 2. Yes, and don't ask again for this command\n  3. No, and tell the agent what to do differently\n";
    let agent = json!({"terminal": "app:t48", "space": {"id": "sp-api", "name": "api"}, "tab": {"id": "tb-417", "name": "4"}, "title": "Retry failed CSV exports",
                       "program": "codex", "status": "blocked", "since_min": 2, "cwd": "/Users/me/code/api", "screen": screen});
    std::fs::write(&world, json!({"agents": [agent]}).to_string()).unwrap();
    // SAFETY: this test binary holds this one test, so nothing reads the environment meanwhile.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("COCKPIT_AGENTS_DEMO", &world);
    }
    // These answers are the person's: the test says so, as only cockpit's own code may.
    cockpit_core::peer::trusted();
    let view = gui::view(&json!({}), &|_| Value::Null);
    let q = &view["agents"][0]["question"];
    assert_eq!(q["kind"], "text", "its menu does not read as one: {q}");
    let asked = q["asked"].as_str().unwrap().to_string();
    let answer = gui::act("answer", &[json!({"terminal": "app:t48", "text": "go ahead", "asked": asked, "source": "test:click"})]).unwrap();
    assert_eq!(answer, Err("Retry failed CSV exports shows a menu cockpit cannot read: answer it in its terminal".into()));
    let mut sent = false;
    let typed = gui::waiting("app:t48", &mut || {
        sent = true;
        true
    });
    assert_eq!(typed, Err("Retry failed CSV exports is waiting on a question: answer it first".into()), "named as the views title it");
    assert!(!sent, "nothing was typed");
}
