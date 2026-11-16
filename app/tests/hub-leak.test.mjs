// request() gives up on a connection that is too slow, and closes it when it comes later.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import net from "node:net";
import { once } from "node:events";
import { createContext, SourceTextModule, SyntheticModule } from "node:vm";
import { test } from "node:test";

const HUB = new URL("../mods/cockpit/hub.js", import.meta.url);

async function fixture(t, { connectDelay = 0, stopped = false } = {}) {
  const clients = new Set(), calls = [];
  const server = net.createServer((socket) => {
    clients.add(socket);
    socket.on("close", () => clients.delete(socket));
    let pending = "";
    socket.on("data", (d) => {
      pending += d;
      while (pending.includes("\n")) {
        const end = pending.indexOf("\n");
        const req = JSON.parse(pending.slice(0, end));
        pending = pending.slice(end + 1);
        socket.write(JSON.stringify(req.op === "ping" ? { ok: true } : { value: {} }) + "\n");
      }
    });
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => { for (const s of clients) s.destroy(); server.close(); });
  const info = { host: "127.0.0.1", port: server.address().port, token: "tok" };
  const install = { bin: "/x/cockpit", home: "/x", cache: "/x/cache" };
  const context = createContext({ console });
  const imports = {
    herdr: {
      appHome: () => "/app",
      window: () => ({ platform: "macos" }),
      run: () => {},
      readTextFile: async (path) => {
        if (path === "/app/cockpit.json") return JSON.stringify(install);
        if (path.endsWith("/hub.stopped")) return stopped ? "" : null;
        return JSON.stringify(info);
      },
      exec: async (...a) => { calls.push(a); return { code: 0, stdout: JSON.stringify(info), stderr: "" }; },
    },
    net: { default: { connect: async (host, port) => {
      if (connectDelay) await new Promise((r) => setTimeout(r, connectDelay));
      const socket = net.createConnection({ host, port });
      await once(socket, "connect");
      const chunks = (async function* () { for await (const c of socket) yield c; })();
      return {
        write: (d) => new Promise((res, rej) => socket.write(d, (e) => (e ? rej(e) : res()))),
        read: async () => { const c = await chunks.next(); return c.done ? null : c.value; },
        close: () => socket.destroy(),
      };
    } } },
  };
  const module = new SourceTextModule(await readFile(HUB, "utf8"), { context });
  await module.link((name) => new SyntheticModule(Object.keys(imports[name]), function () {
    for (const [k, v] of Object.entries(imports[name])) this.setExport(k, v);
  }, { context }));
  await module.evaluate();
  return { hub: module.namespace, clients, calls };
}

test("request(): a connection that comes after its 20 s bound gave up is closed, not left open", async (t) => {
  const f = await fixture(t, { connectDelay: 50 });
  // A cx whose 20 s timer (request's outer bound) fires at once and whose 10 s timers (open's own steps) never do.
  const cx = { timer: { after: (ms, fn) => { const id = ms === 20_000 ? setTimeout(fn, 1) : null; return { cancel: () => id && clearTimeout(id) }; } } };
  await assert.rejects(f.hub.act("inbox", "set_muted", ["api", true], cx), /did not answer/);
  // Let the late connect and its ping finish.
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(f.clients.size, 0, `a connection given up on should be closed, ${f.clients.size} still open`);
});

// The mod loaded again and again in one window, as every save and every update does: each load is hub.js evaluated
// afresh in the same context, its tasks left waiting on their reads as a reload leaves them.
async function loads(t, n, { connectDelay = () => 0 } = {}) {
  const clients = new Set();
  const server = net.createServer((socket) => {
    clients.add(socket);
    socket.on("close", () => clients.delete(socket));
    let pending = "";
    socket.on("data", (d) => {
      pending += d;
      while (pending.includes("\n")) {
        const end = pending.indexOf("\n");
        const req = JSON.parse(pending.slice(0, end));
        pending = pending.slice(end + 1);
        // A subscription gets its first value and then waits, as one does between changes.
        socket.write(JSON.stringify(req.op === "ping" ? { ok: true } : { view: "agents", value: { agents: [] } }) + "\n");
      }
    });
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => { for (const s of clients) s.destroy(); server.close(); });
  const info = { host: "127.0.0.1", port: server.address().port, token: "tok" };
  const install = { bin: "/x/cockpit", home: "/x", cache: "/x/cache" };
  const context = createContext({ console });
  const source = await readFile(HUB, "utf8");
  const namespaces = [];
  for (let i = 0; i < n; i++) {
    const delay = connectDelay(i);
    const imports = {
      herdr: {
        appHome: () => "/app",
        window: () => ({ platform: "macos" }),
        run: () => {},
        readTextFile: async (path) => (path === "/app/cockpit.json" ? JSON.stringify(install) : path.endsWith("/hub.stopped") ? null : JSON.stringify(info)),
        exec: async () => ({ code: 0, stdout: JSON.stringify(info), stderr: "" }),
      },
      net: { default: { connect: async (host, port) => {
        if (delay) await new Promise((r) => setTimeout(r, delay));
        const socket = net.createConnection({ host, port });
        await once(socket, "connect");
        const chunks = (async function* () { for await (const c of socket) yield c; })();
        return {
          write: (d) => new Promise((res, rej) => socket.write(d, (e) => (e ? rej(e) : res()))),
          read: async () => { const c = await chunks.next(); return c.done ? null : c.value; },
          close: () => socket.destroy(),
        };
      } } },
    };
    const module = new SourceTextModule(source, { context, identifier: `hub.js#${i + 1}` });
    await module.link((name) => new SyntheticModule(Object.keys(imports[name]), function () {
      for (const [k, v] of Object.entries(imports[name])) this.setExport(k, v);
    }, { context }));
    await module.evaluate();
    // A copy's context: tasks run, timers never fire (a reload stops the older load's), no beat.
    const cx = { spawn: (f) => f(cx), notify() {}, timer: { after: () => ({ cancel() {} }) } };
    let live = false;
    module.namespace.subscribe(cx, { views: ["agents"] }, () => (live = true));
    module.namespace.subscribe(cx, { views: ["queue"] }, () => {});
    namespaces.push({ hub: module.namespace, live: () => live });
  }
  return { clients, namespaces };
}

const settle = async (clients, want, ms = 2000) => {
  const until = Date.now() + ms;
  while (clients.size !== want && Date.now() < until) await new Promise((r) => setTimeout(r, 20));
};

test("ten loads of the mod in one window leave only the newest load's connections open", async (t) => {
  const { clients, namespaces } = await loads(t, 10);
  await settle(clients, 2);
  assert.equal(clients.size, 2, `the newest load's two subscriptions alone should be open, ${clients.size} are`);
  assert.equal(namespaces.at(-1).live(), true, "and the newest one is live");
});

test("a connection an older load asked for that comes after a newer load took over is closed at once", async (t) => {
  // The first load's connects take 300 ms: the second load is there before they come back.
  const { clients } = await loads(t, 2, { connectDelay: (i) => (i === 0 ? 300 : 0) });
  await new Promise((r) => setTimeout(r, 600));
  await settle(clients, 2);
  assert.equal(clients.size, 2, `only the second load's connections should be open, ${clients.size} are`);
});
