import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { createContext, SourceTextModule, SyntheticModule } from "node:vm";
import { test } from "node:test";

const REPORT = {
  version: 1, at: 1793611507779, cockpit: "0.40.3", summary: { pass: 1, warn: 1, fail: 1 },
  checks: [
    { id: "tools.gh", category: "tools", status: "fail", message: "gh not signed in", fix: { label: "gh auth login", command: "gh auth login" } },
    { id: "sources.cache.inbox", category: "sources", status: "warn", message: "inbox: no data yet", fix: { label: "open the inbox", act: { app: "sources", action: "refresh", args: [] } } },
    { id: "claude.plugin", category: "claude", status: "pass", message: "Claude plugin cockpit 0.40.3", fix: null },
  ],
  getting_started: [
    { id: "setup", label: "Set up cockpit", done: true, check: "tools.config", fix: null },
    { id: "github", label: "Sign in to GitHub", done: false, check: "tools.gh", fix: { label: "gh auth login", command: "gh auth login" } },
    { id: "tools", label: "Give Claude Code cockpit's tools", done: false, check: "claude.plugin", fix: { label: "/plugin install cockpit" } },
    { id: "tickets", label: "Connect your tickets", done: false, check: null, fix: { label: "cockpit setup", command: "cockpit setup" } },
  ],
  agents: [],
};

/** The hub's `options` view: one option seen, one not. */
const OPTIONS = [
  { key: "queue.drain", since: "0.41.0", title: "Send queued prompts on their own", description: "A prompt goes once the agent rests.", default: true, kind: "toggle", value: true, seen: true },
  { key: "chat.nextSteps", since: "0.41.0", title: "Suggest next prompts", description: "A few prompts after each turn.", default: false, kind: "toggle", value: false, seen: false },
];

/**
 * doctor.js on a fake hub: `cockpit` answers with `stdout`, or each run with the next of `answers` (a function, so a test
 * can hold one back); `act` records its calls.
 */
async function fixture(stdout = JSON.stringify(REPORT), answers = null) {
  const acts = [], ran = [];
  const context = createContext({ console });
  const imports = {
    herdr: { shared: () => "cockpit", setShared: () => null, props: () => ({}) },
    "./hub.js": {
      cockpit: async (args) => (ran.push(args), answers ? answers.shift()(args) : { code: 0, stdout, stderr: "" }),
      act: async (...args) => (acts.push(args), { note: "running gh auth login in the actions space" }),
      view: async () => ({ options: OPTIONS }),
    },
  };
  const sources = new Map(), modules = new Map();
  const folder = new URL("../mods/cockpit/", import.meta.url);
  for (const name of await readdir(folder)) {
    if (!name.endsWith(".js")) continue;
    const text = await readFile(new URL(name, folder), "utf8");
    sources.set(`./${name}`, text);
    for (const match of text.matchAll(/import\s*\{([^}]+)\}\s*from\s*"(herdr|gpui-kit|gpui-base)"/g)) {
      const exports = imports[match[2]] ??= {};
      for (const word of match[1].split(",")) {
        const key = word.trim().split(/\s+as\s+/)[0];
        exports[key] ??= () => { throw new Error(`unexpected native call: ${key}`); };
      }
    }
  }
  function load(name) {
    if (modules.has(name)) return modules.get(name);
    const native = imports[name];
    const module = native
      ? new SyntheticModule(Object.keys(native), function () {
        for (const [key, value] of Object.entries(native)) this.setExport(key, value);
      }, { context })
      : new SourceTextModule(sources.get(name), { context, identifier: name });
    modules.set(name, module);
    return module;
  }
  const module = new SourceTextModule('export * from "./doctor.js"; export { default } from "./doctor.js";', { context });
  await module.link(load);
  await module.evaluate();
  return { doctor: module.namespace, acts, ran };
}

/** Values made in the module's own realm, as plain ones to compare. */
const plain = (v) => JSON.parse(JSON.stringify(v));

/** A context whose spawned work runs at once, its clipboard and toasts kept. */
function cx() {
  const copied = [], tasks = [];
  return { copied, tasks, write_to_clipboard: (t) => copied.push(t), spawn: (f) => tasks.push(f(cx())), notify: () => {} };
}

test("the report is read from cockpit doctor --json, cached when asked", async () => {
  const f = await fixture();
  const report = await f.doctor.loadDoctor({ cached: true });
  assert.deepEqual(plain(f.ran), [["doctor", "--json", "--cached"]]);
  assert.equal(report.summary.fail, 1);
  assert.deepEqual(plain(f.doctor.toDo(report).map((s) => s.id)), ["github", "tools", "tickets"]);
  const broken = await fixture("");
  await assert.rejects(broken.doctor.loadDoctor(), /cockpit doctor did not answer/);
});

test("a fix runs through the hub on the report it was shown in, and words are copied", async () => {
  const f = await fixture();
  assert.equal(f.doctor.fixVerb(REPORT.checks[0].fix), "Run");
  assert.equal(f.doctor.fixVerb(REPORT.checks[1].fix), "Fix");
  assert.equal(f.doctor.fixVerb(REPORT.getting_started[2].fix), null);
  const toasts = { pushed: [], push: (_cx, title) => toasts.pushed.push(title) };
  const c = cx();
  let done = 0;
  f.doctor.runFix(REPORT.getting_started[1], REPORT, c, toasts, () => done++);
  await Promise.all(c.tasks);
  assert.deepEqual(plain(f.acts), [["doctor", "fix", ["tools.gh", REPORT.at, "gh auth login"]]], "a step fixes through its check, naming the report and the fix shown");
  assert.deepEqual(toasts.pushed, ["running gh auth login in the actions space"]);
  assert.equal(done, 1, "the caller hears the hub took it, to check again");
  f.doctor.runFix(REPORT.getting_started[2], REPORT, c, toasts);
  assert.deepEqual(c.copied, ["/plugin install cockpit"]);
  assert.equal(f.acts.length, 1, "nothing runs a fix said in words");
});

test("a step no check says is copied, never sent as a check", async () => {
  const f = await fixture();
  const step = REPORT.getting_started[3];
  assert.equal(f.doctor.runnable(step), false);
  assert.equal(f.doctor.runnable(REPORT.getting_started[1]), true);
  const c = cx();
  f.doctor.runFix(step, REPORT, c, { push: () => {} });
  await Promise.all(c.tasks);
  assert.deepEqual(plain(c.copied), ["cockpit setup"]);
  assert.equal(f.acts.length, 0);
});

test("a fix says what it will do before it runs", async () => {
  const f = await fixture();
  assert.deepEqual(plain(f.doctor.fixQuestion(REPORT.checks[0])), {
    title: "Run this command?", description: "gh auth login runs in a tab of the actions space, where you can watch it.", confirm: "Run",
  });
  assert.deepEqual(plain(f.doctor.fixQuestion(REPORT.checks[1])), { title: "Fix it?", description: "Reads every source again now.", confirm: "Fix" });
  assert.equal(f.doctor.shownFix(REPORT.checks[1].fix), 'sources refresh []', "an action as the hub compares it");
});

test("a confirm asked from one report closes when another lands, and never runs that one's fix", async () => {
  const A = { ...REPORT, at: 1000, checks: [{ id: "install.link", category: "install", status: "fail", message: "no link", fix: { label: "ln -sfn /a ~/.config/cockpit/herdr", command: "ln -sfn /a ~/.config/cockpit/herdr" } }] };
  const B = { ...A, at: 2000, checks: [{ ...A.checks[0], fix: { label: "sh /b/ensure-spaces.sh", command: "sh /b/ensure-spaces.sh" } }] };
  let release;
  const held = new Promise((r) => (release = r));
  const f = await fixture(undefined, [async () => ({ code: 0, stdout: JSON.stringify(A), stderr: "" }), async () => (await held, { code: 0, stdout: JSON.stringify(B), stderr: "" })]);
  const tasks = [];
  const cx = { spawn: (fn) => tasks.push(fn(cx)), notify: () => {}, focus_handle: () => ({ focus() {} }), write_to_clipboard() {} };
  const sheet = new f.doctor.default();
  const toasts = [];
  sheet.init({}, cx);
  sheet.toasts = { push: (_cx, title) => toasts.push(title), render: () => null };
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(sheet.report.at, 1000, "the kept report shows first");
  sheet.ask(sheet.report.checks[0], cx);
  assert.equal(f.doctor.fixQuestion(sheet.asking.item).description, "ln -sfn /a ~/.config/cockpit/herdr runs in a tab of the actions space, where you can watch it.");
  release();
  await Promise.all(tasks);
  assert.equal(sheet.report.at, 2000);
  assert.equal(sheet.asking, null, "the question asked from the old report is gone");
  assert.deepEqual(toasts, ["The checks ran again: look again"]);
  sheet.confirmed(cx);
  await Promise.all(tasks);
  assert.deepEqual(plain(f.acts), [], "nothing runs");
  // Asked and confirmed from the report shown, the hub is told that report and that fix.
  sheet.ask(sheet.report.checks[0], cx);
  sheet.confirmed(cx);
  await Promise.all(tasks);
  assert.deepEqual(plain(f.acts), [["doctor", "fix", ["install.link", 2000, "sh /b/ensure-spaces.sh"]]]);
});

test("the options tab notes what it shows as seen and sets an option from the person's own key only", async () => {
  const f = await fixture();
  const tasks = [];
  const cx = { spawn: (fn) => tasks.push(fn(cx)), notify: () => {}, focus_handle: () => ({ focus() {} }), write_to_clipboard() {} };
  const sheet = new f.doctor.default();
  sheet.init({ tab: "options" }, cx);
  const toasts = [];
  sheet.toasts = { push: (_cx, title, o) => toasts.push([title, o?.action?.label]), render: () => null };
  await Promise.all(tasks);
  assert.equal(sheet.tab, "options", "opened on the tab asked for");
  const optionActs = () => plain(f.acts.filter(([app]) => app === "options").map(([app, action, args]) => [app, action, args]));
  assert.deepEqual(optionActs(), [["options", "shown", [{ keys: ["chat.nextSteps"] }]]], "the one not seen yet, once");
  sheet.key({ keystroke: "j" }, cx);
  sheet.key({ keystroke: "space", injected: true }, cx);
  await Promise.all(tasks);
  assert.equal(optionActs().length, 1, "a key from outside the window sets nothing");
  sheet.key({ keystroke: "space" }, cx);
  await Promise.all(tasks);
  assert.deepEqual(optionActs().at(-1), ["options", "set", [{ "chat.nextSteps": true }]]);
  assert.deepEqual(toasts, [["Suggest next prompts: on", "Undo"]]);
  assert.equal(sheet.options.find((o) => o.key === "chat.nextSteps").value, true);
});
