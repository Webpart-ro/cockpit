// Diagnostics: `cockpit doctor --json` as a sheet over the window (every check with its fix, the agents and what each
// can do, every option with its switch), and the "Getting started" checklist its checks make, for the empty states. A
// fix, once confirmed, runs through the hub (`act doctor fix`), which runs the doctor's own command or action, never one
// this sends; an option is set by the person's click or key only (`act options set`).

import { div } from "gpui-kit";
import * as base from "gpui-base";
import { h_flex, v_flex } from "gpui-base";
import { props, shared, setShared, openOverlay, closeOverlay } from "herdr";
import * as hub from "./hub.js";
import { tokens, TEXT, RADIUS, Icon, Button, Badge, Tabs, Skeleton, Spinner, Toaster, Mono, Card, AlertDialog, Toggle, AgentLogo } from "./ui.js";

// A Herdr without ScrollHandle still draws the sheet; j/k then move without scrolling.
const ScrollHandle = base.ScrollHandle ?? null;
/** How many checks fail in the last report read, shared so every sidebar and drawer redraws its link. */
const FAILING = "cockpit.doctor.failing";

/** The categories of checks, in the order the sheet lists them. */
const CATEGORIES = {
  tools: "Tools and config",
  install: "Install",
  app: "Herdr app",
  agents: "Agents",
  claude: "Claude Code plugin",
  sessions: "Sessions",
  sources: "Sources",
  connectors: "Connectors",
  rules: "Permission rules",
  spaces: "Spaces",
  notifications: "Notifications",
  browser: "Browser for E2E tests",
};
const CAN = [["start", "start"], ["resume", "resume"], ["fork", "fork"], ["headless", "headless"], ["driven", "driven"]];

/** The doctor's report, `--cached` for one under ten minutes old; throws when it does not run. */
export async function loadDoctor({ cached = false } = {}) {
  const r = await hub.cockpit(["doctor", "--json", ...(cached ? ["--cached"] : [])]);
  let report;
  try {
    report = JSON.parse(r.stdout);
  } catch {
    throw new Error((r.stderr || r.stdout || "cockpit doctor did not answer").trim().split("\n")[0]);
  }
  const failing = report?.summary?.fail ?? 0;
  if (shared(FAILING) !== failing) setShared(FAILING, failing);
  return report;
}

/** Opens the Diagnostics sheet: a window over the cockpit, on `tab` when given (problems, all, agents, options). */
export function openDiagnostics({ tab = null } = {}) {
  openOverlay({ mod: "cockpit", kind: "window", key: "cockpit-diagnostics", width: 860, height: 0.86, props: { app: "diagnostics", tab } });
}

/** The tabs in order, by their number key. */
const TABS = ["problems", "all", "agents", "options"];

/**
 * A quiet row that opens the sheet, with how many checks fail when a report was read: the sidebar's and the drawer's.
 * `rail` keeps the icon alone.
 */
export function DiagnosticsLink(id, { rail = false } = {}) {
  // A remote desk reads another machine: its checks are that machine's to run.
  if (hub.remoteMachine()) return div();
  const k = tokens();
  const failing = Number(shared(FAILING) ?? 0);
  const tip = `Diagnostics${failing ? `: ${failing} failing` : ""}`;
  const row = h_flex()
    .id(id)
    .h(28)
    .gap(8)
    .flex_none()
    .items_center()
    .rounded(RADIUS.md)
    .cursor_pointer()
    .text_color(k.mutedFg)
    .hover((s) => s.bg(k.accent).text_color(k.foreground))
    .on_click(() => openDiagnostics())
    .tooltip(chat() ? tip : `${tip}  ⌘⇧P, then Diagnostics`)
    .child(Icon("shield-check", { size: 14, color: failing && !chat() ? k.destructive : undefined }));
  if (rail) return row.justify_center();
  return row
    .px(8)
    .child(div().flex_1().truncate().text_size(TEXT.sm).child("Diagnostics"))
    .when(failing > 0, (e) => e.child(div().text_size(TEXT.xs).font_medium().text_color(chat() ? k.foreground : k.destructive).child(`${failing} failing`)));
}

/** The steps not done yet, in order. */
export const toDo = (report) => (report?.getting_started ?? []).filter((s) => !s.done);

/** What a fix runs, for its button: "Fix" when the hub can, null when it only says what to do. */
export const fixVerb = (fix) => (fix?.act ? "Fix" : fix?.command ? "Run" : null);

/** The check a fix goes through: a step names its own (null when no check says it), a check is itself. */
const checkOf = (item) => ("check" in item ? item.check : item.id);

/** Whether the hub can run this item's fix: a check of the report with a command or an action. */
export const runnable = (item) => Boolean(fixVerb(item?.fix) && checkOf(item));

/** A fix as the person saw it, as the hub compares it: its command, or `app action [args]`. */
export const shownFix = (fix) => fix?.command ?? `${fix?.act?.app ?? ""} ${fix?.act?.action ?? ""} ${JSON.stringify(fix?.act?.args ?? [])}`;

/** The hub actions a fix may name, in plain words. */
const ACTION_WORDS = {
  "sources refresh": () => "Reads every source again now.",
  "sources sign_in": (args) => `Opens the sign-in to ${args?.[0] ?? "that source"} in a tab of the actions space.`,
};

/** The question before a fix runs: `{title, description, confirm}`. */
export function fixQuestion(item) {
  const fix = item.fix ?? {};
  if (fix.act) {
    const words = ACTION_WORDS[`${fix.act.app} ${fix.act.action}`];
    return { title: "Fix it?", description: words ? words(fix.act.args) : `Runs ${fix.act.app} ${fix.act.action}.`, confirm: "Fix" };
  }
  return { title: "Run this command?", description: `${fix.command} runs in a tab of the actions space, where you can watch it.`, confirm: "Run" };
}

/**
 * Runs a confirmed fix through the hub, naming the report it was shown in (`report.at`) and the fix as shown, so the hub
 * runs that one or nothing; copies what it says when nothing runs it. The outcome is `toasts`' toast, and `done(cx)`
 * follows once the hub took it.
 */
export function runFix(item, report, cx, toasts, done) {
  const fix = item.fix;
  if (!runnable(item)) {
    const words = fix?.command ?? fix?.label;
    if (words) {
      cx.write_to_clipboard(words);
      toasts?.push(cx, "Copied what to do", { kind: "info", description: words });
    }
    return;
  }
  cx.spawn(async (cx) => {
    try {
      const r = await hub.act("doctor", "fix", [checkOf(item), report?.at ?? 0, shownFix(fix)]);
      toasts?.push(cx, r.error || r.note || "Fixing", { kind: r.error ? "error" : "info" });
      if (!r.error) done?.(cx);
    } catch (e) {
      toasts?.push(cx, String(e?.message ?? e), { kind: "error" });
    }
  });
}

/** Cockpit Chat's quiet confirm: a plain card, Cancel holding the keyboard, no keys drawn. */
function quietConfirm(id, { title, description, confirmLabel, onConfirm, onCancel }) {
  const k = tokens();
  return div()
    .id(id)
    .absolute()
    .inset_0()
    .bg("#00000066")
    .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
    .child(
      v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(
          Card({
            id: `${id}-card`,
            title,
            description,
            padding: 24,
            gap: 16,
            footer: h_flex()
              .justify_end()
              .gap(8)
              .child(Button(`${id}-cancel`, { label: "Cancel", variant: "ghost", tooltip: "Leave it  esc", onClick: onCancel }))
              .child(Button(`${id}-ok`, { label: confirmLabel, variant: "outline", onClick: onConfirm })),
          })
            .w(420)
            .bg(k.popover)
            .shadow_lg(),
        ),
    );
}

/** The confirm before a fix, as each layout asks it: Cockpit's AlertDialog, Cockpit Chat's quiet card. */
function fixConfirm(id, item, { onConfirm, onCancel }) {
  const q = fixQuestion(item);
  const ask = { title: q.title, description: q.description, confirmLabel: q.confirm, onConfirm, onCancel };
  return chat() ? quietConfirm(id, ask) : AlertDialog(id, ask);
}

const chat = () => shared("layout") === "cockpit-chat";
const statusIcon = (status) => (status === "pass" ? "circle-check" : status === "fail" ? "circle-x" : "triangle-alert");
// Cockpit Chat keeps colour for agents' states: there the icons' shapes say it, in grey.
const statusColor = (k, status) =>
  chat() ? (status === "pass" ? k.dim : status === "fail" ? k.foreground : k.mutedFg) : status === "pass" ? k.success : status === "fail" ? k.destructive : k.warning;

/** One step of getting started: done or not, and its fix beside it, or the question before it runs. */
function stepRow(k, step, { onFix, compact, asking, onConfirm, onCancel }) {
  const fix = step.fix;
  if (asking === step.id) {
    const q = fixQuestion(step);
    return h_flex()
      .id(`start-${step.id}`)
      .gap(8)
      .min_h(compact ? 28 : 32)
      .items_center()
      .child(Icon("circle-dashed", { size: 15, color: k.mutedFg }))
      .child(div().flex_1().min_w(0).truncate().text_size(TEXT.sm).text_color(k.foreground).child(q.description))
      .child(Button(`start-cancel-${step.id}`, { label: "Cancel", variant: "ghost", size: "sm", onClick: (_e, cx) => onCancel(cx) }))
      .child(Button(`start-ok-${step.id}`, { label: q.confirm, size: "sm", variant: "outline", onClick: (_e, cx) => onConfirm(step, cx) }));
  }
  return h_flex()
    .id(`start-${step.id}`)
    .gap(10)
    .min_h(compact ? 28 : 32)
    .items_center()
    .child(Icon(step.done ? "circle-check" : "circle-dashed", { size: 15, color: step.done ? statusColor(k, "pass") : k.mutedFg }))
    .child(div().flex_1().min_w(0).truncate().text_size(TEXT.sm).text_color(step.done ? k.mutedFg : k.foreground).when(step.done, (e) => e.line_through()).child(step.label))
    .when(!step.done && Boolean(fix), (e) =>
      e.child(
        Button(`start-fix-${step.id}`, {
          label: runnable(step) ? fixVerb(fix) : "Copy",
          icon: runnable(step) ? "play" : "copy",
          size: "sm",
          variant: "outline",
          tooltip: fix.command ?? fix.label,
          onClick: (_e, cx) => onFix(step, cx),
        }),
      ),
    );
}

/**
 * The "Getting started" checklist of a doctor report: each step done or not, with its fix; null when every step is
 * done. `onFix(step, cx)` asks before a step's fix runs; `asking` is the step being asked about, which `onConfirm(step,
 * cx)` runs and `onCancel(cx)` leaves.
 */
export function StartedList(report, { onFix, compact = false, title = "Getting started", asking = null, onConfirm, onCancel } = {}) {
  const steps = report?.getting_started ?? [];
  const left = steps.filter((s) => !s.done).length;
  if (!steps.length || !left) return null;
  const k = tokens();
  return v_flex()
    .id("getting-started")
    .w_full()
    .max_w(compact ? 520 : 640)
    .gap(4)
    .p(compact ? 14 : 16)
    .rounded(RADIUS.lg)
    .border_1()
    .border_color(k.border)
    .bg(k.card)
    .child(
      h_flex()
        .gap(8)
        .items_center()
        .pb(4)
        .child(div().flex_1().text_size(TEXT.base).font_semibold().text_color(k.foreground).child(title))
        .child(div().text_size(TEXT.xs).text_color(k.mutedFg).child(`${steps.length - left} of ${steps.length} done`)),
    )
    .children(steps.map((s) => stepRow(k, s, { onFix, compact, asking, onConfirm, onCancel })));
}

/**
 * A self-contained "Getting started" for an empty state: once the empty state shows it reads the kept doctor report (or
 * runs one), then draws `StartedList`, or nothing when every step is done. Make it in `init`, call `want(cx)` from
 * `render` while the empty state shows, and draw `el()` there: `this.started = new Started()` …
 * `this.started.want(cx)` … `this.started.el({ compact: true })`.
 */
export class Started {
  constructor() {
    this.report = null;
    this.loading = false;
    this.loaded = false;
    /** The step asked about, with the report it was shown from. */
    this.asking = null;
    this.toasts = new Toaster();
  }

  /** Reads the report the first time the empty state shows, and again after a fix (`cached: false`). */
  want(cx, { cached = true } = {}) {
    if (this.loading || (this.loaded && cached)) return;
    this.loading = true;
    cx.spawn(async (cx) => {
      try {
        this.report = await loadDoctor({ cached });
      } catch {
        this.report = null;
      }
      // A question asked from another report is not this one's.
      if (this.asking && this.asking.at !== this.report?.at) this.asking = null;
      this.loading = false;
      this.loaded = true;
      cx.notify();
    });
  }

  /** The checklist, with this component's own toasts, or null. */
  el({ compact = true, title } = {}) {
    const list = StartedList(this.report, {
      compact,
      title,
      asking: this.asking?.step.id ?? null,
      onFix: (step, cx) => {
        if (!runnable(step)) return runFix(step, this.report, cx, this.toasts);
        this.asking = { step, at: this.report?.at };
        cx.notify();
      },
      onCancel: (cx) => ((this.asking = null), cx.notify()),
      onConfirm: (_step, cx) => {
        const { step, at } = this.asking ?? {};
        this.asking = null;
        // An action is done when the hub answers: read again; a command runs on in its tab, the list waits.
        if (step) runFix(step, { at }, cx, this.toasts, (cx) => (step.fix?.act ? this.want(cx, { cached: false }) : null));
        cx.notify();
      },
    });
    if (!list) return null;
    const toasts = this.toasts.render();
    return toasts ? div().relative().w_full().child(list).child(toasts) : list;
  }
}

export default class Diagnostics {
  init(p, cx) {
    this.p = p ?? props() ?? {};
    this.report = null;
    this.error = null;
    this.running = false;
    this.tab = TABS.includes(this.p.tab) ? this.p.tab : "problems";
    this.at = 0;
    /** The hub's `options` view, and the option selected among them. */
    this.options = null;
    this.optionAt = 0;
    this.setting = false;
    /** The fix asked about, with the report it was shown from. */
    this.asking = null;
    /** A command a fix typed into a tab runs on: the sheet keeps its report until checked again. */
    this.fixing = null;
    this.rowAt = {};
    this.toasts = new Toaster();
    this.handle = cx.focus_handle();
    this.handle.focus();
    this.scroll = ScrollHandle ? ScrollHandle.new() : null;
    this.check(cx, true);
    this.readOptions(cx);
  }

  /** Every option with its value, from the hub; the ones not seen yet count as seen once their tab shows them. */
  readOptions(cx) {
    cx.spawn(async (cx) => {
      let view = null;
      try {
        view = await hub.view("options");
      } catch {}
      this.options = view && !view.error ? view.options ?? [] : [];
      this.seenShown();
      cx.notify();
    });
  }

  /** The options tab in front: what it lists is noted as seen, and keeps its "new" marks while the sheet is open. */
  seenShown() {
    const unseen = (this.options ?? []).filter((o) => !o.seen && !this.noted?.has(o.key)).map((o) => o.key);
    if (this.tab !== "options" || !unseen.length) return;
    this.noted = new Set([...(this.noted ?? []), ...unseen]);
    hub.act("options", "shown", [{ keys: unseen }]).catch(() => {});
  }

  /** One option set through the hub, from a click or the person's own key; its toast takes it back. */
  setOption(o, on, cx) {
    if (this.setting || !o) return;
    this.setting = true;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const r = await hub.act("options", "set", [{ [o.key]: on }], cx);
        if (r?.error) throw new Error(r.error);
        this.options = (this.options ?? []).map((x) => (x.key === o.key ? { ...x, value: on } : x));
        this.toasts.push(cx, `${o.title}: ${on ? "on" : "off"}`, { kind: "success", ms: 8000, action: { label: "Undo", onClick: (_e, cx) => this.setOption({ ...o, value: on }, !on, cx) } });
      } catch (e) {
        this.toasts.push(cx, "The option was not saved", { kind: "error", description: String(e?.message ?? e) });
      }
      this.setting = false;
      cx.notify();
    });
  }

  /** Shows a tab, j/k starting from its top. */
  show(tab) {
    this.tab = tab;
    this.at = 0;
    this.seenShown();
  }

  /** Reads the kept report first when it is recent, then runs every check again; a run already going is enough. */
  check(cx, firstCached = false) {
    if (this.running) return;
    this.running = true;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        if (firstCached && !this.report) {
          this.report = await loadDoctor({ cached: true }).catch(() => null);
          cx.notify();
        }
        this.report = await loadDoctor();
        this.error = null;
        this.fixing = null;
        // A question asked from another report is not this one's: it closes, and the new checks say what is left.
        if (this.asking && this.asking.at !== this.report?.at) {
          this.asking = null;
          this.toasts.push(cx, "The checks ran again: look again", { kind: "info" });
        }
      } catch (e) {
        this.error = String(e?.message ?? e);
      }
      this.running = false;
      cx.notify();
    });
  }

  close() {
    this.scroll?.release();
    closeOverlay(props()?.overlay);
  }

  /** Asks before `item`'s fix runs, from the report shown; a fix in words is copied at once. */
  ask(item, cx) {
    if (!runnable(item)) return runFix(item, this.report, cx, this.toasts);
    this.asking = { item, at: this.report?.at };
    cx.notify();
  }

  /**
   * The asked fix, run as it was shown. An action is done when the hub answers, so the checks run again; a command
   * runs on in its tab, so the sheet waits for Check again.
   */
  confirmed(cx) {
    const { item, at } = this.asking ?? {};
    this.asking = null;
    if (item) runFix(item, { at }, cx, this.toasts, (cx) => (item.fix?.act ? this.check(cx) : ((this.fixing = item.fix.command), cx.notify())));
    cx.notify();
  }

  /** The checks the shown tab lists, in the order it draws them. */
  shown() {
    const checks = this.report?.checks ?? [];
    if (this.tab === "agents" || this.tab === "options") return [];
    const list = this.tab === "all" ? checks : checks.filter((c) => c.status !== "pass");
    const order = Object.keys(CATEGORIES);
    const rank = (c) => (order.includes(c.category) ? order.indexOf(c.category) : order.length);
    return [...list].sort((a, b) => rank(a) - rank(b));
  }

  key(e, cx) {
    const s = e.keystroke;
    const shown = this.shown();
    if (this.asking) {
      // Cockpit's dialog takes ⏎; Cockpit Chat's quiet one only a click.
      if (s === "escape") this.asking = null;
      else if (s === "enter" && !chat()) return (this.confirmed(cx), true);
      cx.notify();
      return true;
    }
    const options = this.tab === "options" ? (this.options ?? []) : null;
    if (s === "escape") this.close();
    else if (s === "shift-r" || s === "r") this.check(cx);
    else if (["1", "2", "3", "4"].includes(s)) this.show(TABS[Number(s) - 1]);
    else if (options && (s === "j" || s === "down")) this.optionAt = Math.min(this.optionAt + 1, Math.max(0, options.length - 1));
    else if (options && (s === "k" || s === "up")) this.optionAt = Math.max(this.optionAt - 1, 0);
    // A key sent from outside the window (`injected`) sets nothing.
    else if (options && (s === "space" || s === "enter")) {
      if (!e.injected) this.setOption(options[this.optionAt], !options[this.optionAt]?.value, cx);
    }
    else if (s === "j" || s === "down") this.reveal(Math.min(this.at + 1, Math.max(0, shown.length - 1)), shown);
    else if (s === "k" || s === "up") this.reveal(Math.max(this.at - 1, 0), shown);
    else if (s === "enter" && shown[this.at]?.fix) this.ask(shown[this.at], cx);
    else return false;
    cx.notify();
    return true;
  }

  /** Selects the `at`-th shown check and scrolls it into view (its place among the body's rows, as drawn last). */
  reveal(at, shown) {
    this.at = at;
    const row = this.rowAt[shown[at]?.id];
    if (row !== undefined) this.scroll?.scroll_to_item(row);
  }

  checkRow(k, c, cx, selected) {
    const verb = fixVerb(c.fix);
    return h_flex()
      .id(`check-${c.id}`)
      .gap(10)
      .py(6)
      .px(8)
      .mx(-8)
      .rounded(RADIUS.md)
      .when(selected, (e) => e.bg(k.selected))
      .items_start()
      .child(div().pt(1).child(Icon(statusIcon(c.status), { size: 15, color: statusColor(k, c.status) })))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(div().text_size(TEXT.sm).text_color(k.foreground).child(c.message))
          .when(Boolean(c.fix?.label) && c.status !== "pass", (e) => e.child(div().text_size(TEXT.xs).text_color(k.mutedFg).child(c.fix.label))),
      )
      .when(c.status !== "pass" && Boolean(c.fix), (e) =>
        e.child(
          Button(`fix-${c.id}`, {
            label: verb ?? "Copy",
            icon: verb ? "play" : "copy",
            size: "sm",
            variant: c.status === "fail" && verb ? "default" : "outline",
            // ⏎ runs the selected check's fix: its key shows there, a Kbd in Cockpit and in the tooltip in Cockpit Chat.
            kbd: selected ? "⏎" : undefined,
            tooltip: c.fix.act ? c.fix.label : verb ? `Runs ${c.fix.command} in a tab of the actions space` : "Copy what to do",
            onClick: (_e, cx) => this.ask(c, cx),
          }),
        ),
      );
  }

  /** The checks as rows, each category's heading before its own; `this.rowAt` notes where each check lands after `first` rows. */
  checksEl(k, list, cx, first = 0) {
    this.rowAt = {};
    if (!list.length) {
      return [
        v_flex()
          .items_center()
          .gap(8)
          .p(32)
          .child(Icon("circle-check", { size: 22, color: chat() ? k.mutedFg : k.success }))
          .child(div().text_size(TEXT.sm).text_color(k.mutedFg).child("Nothing needs a look: every check passed.")),
      ];
    }
    const groups = Object.keys(CATEGORIES).map((cat) => [cat, list.filter((c) => c.category === cat)]);
    const others = list.filter((c) => !CATEGORIES[c.category]);
    if (others.length) groups.push(["other", others]);
    const picked = this.shown()[this.at]?.id;
    const rows = [];
    for (const [cat, items] of groups.filter(([, items]) => items.length)) {
      rows.push(div().id(`cat-${cat}`).pt(rows.length ? 14 : 0).pb(2).text_size(TEXT.xs).font_medium().text_color(k.mutedFg).child((CATEGORIES[cat] ?? "Other").toUpperCase()));
      for (const c of items) {
        this.rowAt[c.id] = first + rows.length;
        rows.push(this.checkRow(k, c, cx, c.id === picked));
      }
    }
    return rows;
  }

  /** Every option: what it does, its default and version, its switch; the ones not seen yet marked new. */
  optionsEl(k) {
    const options = this.options;
    if (!options) return v_flex().gap(10).p(24).children([0, 1, 2].map((i) => Skeleton({ h: 16, w: 260 + i * 80 })));
    if (!options.length) return div().p(24).text_size(TEXT.sm).text_color(k.mutedFg).child("This cockpit has no options to set from here: cockpit options lists them in a terminal.");
    const row = (o, i) =>
      h_flex()
        .id(`option-${o.key}`)
        .gap(12)
        .py(10)
        .px(8)
        .mx(-8)
        .items_start()
        .rounded(RADIUS.md)
        .border_1()
        // A ring, not a fill: an off switch's track is the selected row's colour.
        .border_color(i === this.optionAt ? k.ring : k.background)
        .on_click((_e, cx) => ((this.optionAt = i), cx.notify()))
        .child(
          v_flex()
            .flex_1()
            .min_w(0)
            .gap(3)
            .child(
              h_flex()
                .gap(8)
                .items_center()
                .child(div().text_size(TEXT.base).font_medium().text_color(k.foreground).child(o.title))
                .when(!o.seen, (e) => e.child(Badge("new", "outline"))),
            )
            .child(div().whitespace_normal().text_size(TEXT.sm).text_color(k.mutedFg).child(o.description))
            .child(div().text_size(TEXT.xs).text_color(k.dim).child(`${o.default ? "On" : "Off"} by default · since ${o.since} · ${o.key}`)),
        )
        .child(
          div()
            .pt(2)
            .child(
              Toggle(`option-toggle-${o.key}`, Boolean(o.value), (cx) => ((this.optionAt = i), this.setOption(o, !o.value, cx)), {
                tooltip: `${o.value ? "Turn it off" : "Turn it on"}  space`,
              }),
            ),
        );
    return v_flex()
      .gap(2)
      .child(div().pb(6).text_size(TEXT.sm).text_color(k.mutedFg).child("Saved in ~/.config/cockpit/config.json; cockpit options lists them in a terminal."))
      .children(options.map(row));
  }

  agentsEl(k) {
    const agents = this.report?.agents ?? [];
    const row = (a) =>
      v_flex()
        .id(`agent-${a.id}`)
        .gap(6)
        .py(10)
        .border_b_1()
        .border_color(k.border)
        .child(
          h_flex()
            .gap(8)
            .items_center()
            .child(AgentLogo(a.icon ?? "agent", { size: 16, color: a.installed ? k.foreground : k.mutedFg }))
            .child(div().text_size(TEXT.base).font_medium().text_color(a.installed ? k.foreground : k.mutedFg).child(a.name))
            .when(a.default, (e) => e.child(Badge("default", "secondary")))
            .when(!a.installed, (e) => e.child(Badge("not installed", "outline")))
            .child(div().flex_1())
            .when(Boolean(a.path), (e) => e.child(Mono(a.path, { color: k.dim }))),
        )
        .child(
          h_flex()
            .gap(6)
            .flex_wrap()
            .children(CAN.map(([key, word]) => Badge(word, a.can?.[key] ? "secondary" : "outline", { icon: a.can?.[key] ? "check" : "x" })))
            .child(Badge(a.tools ? `cockpit's tools: ${a.tools === "plugin" ? "its plugin" : "MCP"}${a.connected === false ? ", not connected" : ""}` : "no cockpit tools", a.connected === false && !chat() ? "warning" : "outline")),
        )
        .child(
          div()
            .text_size(TEXT.xs)
            .text_color(k.mutedFg)
            .child(`Usage: ${a.usage ?? "not reported"}${a.limits ? ` · limits: ${a.limits}` : ""}`),
        );
    const installed = agents.filter((a) => a.installed);
    const rest = agents.filter((a) => !a.installed);
    return v_flex()
      .gap(0)
      .children(installed.map(row))
      .when(rest.length > 0, (e) => e.child(div().pt(14).pb(2).text_size(TEXT.xs).font_medium().text_color(k.mutedFg).child(`NOT INSTALLED · ${rest.length}`)).children(rest.map(row)));
  }

  render(cx) {
    const k = tokens();
    const r = this.report;
    const checks = r?.checks ?? [];
    const problems = checks.filter((c) => c.status !== "pass");
    const s = r?.summary ?? {};
    const at = r?.at ? new Date(r.at).toTimeString().slice(0, 5) : null;
    const summary = this.fixing
      ? `${this.fixing} runs in its tab: Check again once it is done`
      : r
        ? [s.fail ? `${s.fail} failing` : null, s.warn ? `${s.warn} to look at` : null, `${s.pass ?? 0} passed`, at ? `checked ${at}` : null].filter(Boolean).join(" · ")
        : (this.error ?? "Checking every part of cockpit…");
    // The body's rows are the scroll area's own children, so j/k can bring the selected check into view.
    let rows;
    // The options come from the hub, not the checks: they show while the checks still run.
    if (this.tab === "options") rows = [this.optionsEl(k)];
    else if (!r && !this.error) rows = [v_flex().gap(10).p(24).children([0, 1, 2, 3, 4].map((i) => Skeleton({ h: 16, w: 240 + ((i * 97) % 300) })))];
    else if (this.tab === "agents") rows = [this.agentsEl(k)];
    else {
      const started = this.tab === "problems" ? StartedList(r, { onFix: (step, cx) => this.ask(step, cx) }) : null;
      rows = started ? [div().pb(16).child(started)] : [];
      rows.push(...this.checksEl(k, this.tab === "all" ? checks : problems, cx, rows.length));
    }
    let root = v_flex()
      .id("diagnostics")
      .track_focus(this.handle)
      .size_full()
      .relative()
      .bg(k.background)
      .text_color(k.foreground)
      .on_mouse_down("left", () => this.handle.focus())
      .on_key_down((e, cx) => {
        if (this.key(e, cx)) cx.stop_propagation();
      })
      .child(
        h_flex()
          .h(60)
          .px(20)
          .gap(12)
          .flex_none()
          .items_center()
          .border_b_1()
          .border_color(k.border)
          .child(h_flex().size(32).rounded(RADIUS.lg).items_center().justify_center().border_1().border_color(k.border).bg(k.muted).child(Icon("shield-check", { size: 16, color: k.foreground })))
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(2)
              .child(div().text_size(TEXT.lg).font_semibold().child("Diagnostics"))
              .child(div().truncate().text_size(TEXT.sm).text_color(this.error ? k.destructive : k.mutedFg).child(summary)),
          )
          .when(this.running, (e) => e.child(Spinner({ size: 14, color: k.mutedFg })))
          .child(Button("diag-again", { label: "Check again", icon: "refresh-cw", variant: "outline", size: "sm", kbd: "R", tooltip: "Run every check again", onClick: (_e, cx) => this.check(cx) }))
          .child(Button("diag-close", { icon: "x", variant: "ghost", size: "icon", tooltip: "Close  esc", onClick: () => this.close() })),
      )
      .child(
        h_flex()
          .px(20)
          .py(10)
          .flex_none()
          .child(
            Tabs("diag-tabs", {
              items: [
                { value: "problems", label: "Needs a look", count: problems.length },
                { value: "all", label: "All checks", count: checks.length },
                { value: "agents", label: "Agents", count: (r?.agents ?? []).filter((a) => a.installed).length },
                { value: "options", label: "Options", count: (this.options ?? []).filter((o) => !o.seen).length || null },
              ],
              value: this.tab,
              onChange: (v, cx) => (this.show(v), cx.notify()),
            }),
          ),
      )
      .child(
        ((area) => (this.scroll ? area.track_scroll(this.scroll) : area))(v_flex().id("diag-body").flex_1().min_h(0).px(20).pb(20).overflow_y_scrollbar()).children(rows),
      );
    const toasts = this.toasts.render();
    if (toasts) root = root.child(toasts);
    if (this.asking) {
      root = root.child(
        fixConfirm("diag-confirm", this.asking.item, { onCancel: (_e, cx) => ((this.asking = null), cx.notify()), onConfirm: (_e, cx) => this.confirmed(cx) }),
      );
    }
    return root;
  }
}
