import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { createContext, SourceTextModule, SyntheticModule } from "node:vm";
import { test } from "node:test";

async function fixture(layout = "cockpit") {
  const calls = [], notices = [];
  const context = createContext({ console });
  const imports = {
    herdr: {
      agents: () => [{ address: "t7", tab: "tab7", space: "s1", status: "working", title: "Local agent" }],
      spaces: () => [{ id: "s1", tabs: [{ id: "tab7" }] }],
      panes: () => [{ terminal: "runner/t7", tab: "rtab", status: "done" }, { terminal: "runner/t8", tab: "rtab", status: "idle" }],
      run: (...args) => calls.push(args),
      setShared: (...args) => calls.push(["shared", ...args]),
      shared: key => (key === "layout" ? layout : null),
      notify: notice => notices.push(notice),
    },
    "./hub.js": { act: () => { throw new Error("no hub action expected"); }, openUrl: () => {} },
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
  const module = new SourceTextModule('export * from "./fleet.js"; export { default as Home } from "./home.js"; export { default as Sidebar } from "./sidebar.js"; export { AgentsModel } from "./agentsui.js"; export { default as Needs } from "./needs.js";', { context });
  await module.link(load);
  await module.evaluate();
  return { fleet: module.namespace, calls, notices };
}

test("the agents' screen is the one the shown layout calls it", async () => {
  const cockpit = await fixture();
  cockpit.fleet.goToAgent({ address: "t7" });
  const selects = f => f.calls.filter(c => c[0] === "layout.select").map(c => `${c[1].stack}/${c[1].child}`);
  assert.deepEqual(selects(cockpit).join(" "), "center/work");
  const chat = await fixture("cockpit-chat");
  chat.fleet.goToAgent({ address: "t7" });
  // The reading layout stacks the conversation and the panes inside the screen that holds the side panel.
  assert.deepEqual(selects(chat).join(" "), "screen/work view/chat");
});

test("a runner's t7 stays separate from local t7 and Go focuses the runner's terminal", async () => {
  const f = await fixture();
  const model = { remote: "runner", agents: [{ address: "t7", terminal: "app:t7", frontend: "app", title: "Runner agent", stage: "done", tab: { id: "tab7" } }] };
  const rows = f.fleet.fleet(model, {});
  assert.equal(rows.length, 1);
  assert.equal(rows[0].title, "Runner agent");
  assert.equal(rows[0].address, "runner/t7");
  assert.equal(rows[0].remote, "runner");
  assert.equal(rows[0].status, undefined);
  f.fleet.goToAgent(rows[0]);
  assert.equal(f.calls[0][0], "agent.focus");
  assert.equal(f.calls[0][1].terminal, "runner/t7");
  assert.equal(f.notices.length, 0);
});

test("a runner's agent stays done and unread only while this desktop's Herdr still shows it done", async () => {
  const f = await fixture();
  const model = {
    remote: "runner",
    counts: { unread: 3 },
    agents: [
      { address: "t7", terminal: "app:t7", stage: "done", unread: true },
      { address: "t8", terminal: "app:t8", stage: "done", unread: true },
      { address: "t9", terminal: "app:t9", stage: "done", unread: true },
    ],
  };
  assert.deepEqual(JSON.parse(JSON.stringify(f.fleet.rowsOf(model).map((r) => `${r.address} ${r.stage} ${r.unread}`))), ["runner/t7 done true", "runner/t8 idle false", "runner/t9 done false"]);
  assert.equal(f.fleet.countsOf(model).unread, 1);
  assert.equal(f.fleet.countsOf({ counts: { unread: 3 }, agents: model.agents }).unread, 3);
});

test("local agents retain their metadata join and navigation", async () => {
  const f = await fixture();
  const [row] = f.fleet.fleet({ agents: [{ address: "t7", title: "Local task" }] }, {});
  assert.equal(row.title, "Local task");
  assert.equal(row.status, "working");
  assert.equal(row.tab, "tab7");
  f.fleet.goToAgent(row);
  assert.equal(f.calls[0][0], "agent.focus");
  assert.equal(f.calls[0][1].terminal, "t7");
  assert.equal(f.notices.length, 0);
});

test("remote Go, Peek and Open beside navigate to the runner's terminal; Archive stays refused", async () => {
  const f = await fixture();
  const model = { remote: "runner", agents: [{ address: "t7", terminal: "app:t7", title: "Runner agent", tab: { id: "tab7" } }] };
  const agents = Object.create(f.fleet.AgentsModel.prototype);
  agents.value = model;
  const row = agents.rows[0];
  const cx = { notify() {} };
  assert.equal(new f.fleet.Sidebar().subRows({}, row).length, 0);
  agents.go(cx, row);
  agents.command(`agents|peek|${row.terminal}|`, cx);
  const home = new f.fleet.Home();
  const [agent] = f.fleet.fleet(model, {});
  home.openBeside(cx, agent);
  home.archive(cx, agent);
  home.archiveAll(cx, [{ agent }]);
  assert.deepEqual(f.calls.filter(c => c[1]?.terminal).map(c => `${c[0]} ${c[1].terminal}`), [
    "agent.focus runner/t7",
    "terminal.peek runner/t7",
    "agent.focus runner/t7",
  ]);
  assert.equal(f.calls.some(c => c[0] === "tab.settle"), false);
  assert.equal(f.notices.length, 2);
});

test("local Go, Peek, Open beside and Archive handlers retain their commands", async () => {
  const f = await fixture();
  const model = { agents: [{ address: "t7", terminal: "app:t7", title: "Local task", herdr: true }] };
  const agents = Object.create(f.fleet.AgentsModel.prototype);
  agents.value = model;
  const row = agents.rows[0];
  const cx = { notify() {} };
  agents.go(cx, row);
  agents.command(`agents|peek|${row.terminal}|`, cx);
  const home = new f.fleet.Home();
  home.toasts = { push() {} };
  const [agent] = f.fleet.fleet(model, {});
  home.openBeside(cx, agent);
  home.archive(cx, agent);
  home.archiveAll(cx, [{ agent }]);
  assert.deepEqual(f.calls.map(c => c[0]), ["agent.focus", "layout.select", "shared", "terminal.peek", "agent.focus", "layout.show", "tab.settle", "tab.settle"]);
  assert.equal(f.calls.at(-1)[1].id, "tab7");
  assert.equal(f.notices.length, 0);
});

test("every view reads an agent's status through one helper: error is failed, an unknown word needs a look", async () => {
  const { fleet } = await fixture();
  const read = ["working", "blocked", "failed", "error", "done", "idle", "", null, "compacting"].map(fleet.agentStatus);
  assert.equal(read.join(" "), "working blocked failed failed done idle idle idle blocked");
  assert.equal(fleet.stageOf({ status: "failed" }), "needs", "a failed agent waits on you, beside the blocked ones");
  assert.equal(fleet.stageOf({ hubStage: "failed" }), "needs");
  const [row] = fleet.fleet({ agents: [{ address: "t7", title: "Retry failed CSV exports", interrupted: true, error: "exit 1" }] }, {});
  assert.equal(row.interrupted, true);
  assert.equal(row.error, "exit 1");
});

test("Needs lists the agents asking, then those stopped with an error, and the drawer counts both", async () => {
  const { fleet } = await fixture("cockpit-chat");
  const rows = [
    { terminal: "app:t1", address: "t1", stage: "needs_you", question: { text: "Run the migration?" } },
    { terminal: "app:t2", address: "t2", stage: "failed", status: "failed", error: "API Error: 529 overloaded" },
    { terminal: "app:t3", address: "t3", stage: "working", status: "working" },
    { terminal: "app:t4", address: "t4", stage: "done", status: "error" },
    { terminal: "app:t5", address: "t5", stage: "done", status: "done", interrupted: true },
  ];
  assert.deepEqual([...fleet.needing(rows).map((r) => r.address)], ["t1", "t2", "t4"], "Herdr's raw error is a failure too; stopped by you is not");
  const needs = Object.create(fleet.Needs.prototype);
  needs.model = { value: { agents: rows } };
  assert.deepEqual([...needs.waiting().map((r) => r.address)], ["t1", "t2", "t4"]);
  const home = fleet.fleet({ agents: rows.slice(0, 2) }, null).filter((a) => a.stage === "needs").map((a) => a.address);
  assert.deepEqual([...home.sort()], ["t1", "t2"], "the same agents Home's NEXT MOVE takes");
});
