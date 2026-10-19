// OpenConverter web interface entry point.
import { api } from "./api.js";
import { Card } from "./ui/card.js";
import { announce, clear, h, icon } from "./ui/dom.js";
import { friendlyError } from "./ui/format.js";
import { HistoryView } from "./ui/history.js";
import { SettingsDialog } from "./ui/settings.js";

const STORE_KEY = "oc-queue-v1";
const THEMES = ["system", "light", "dark"];
const THEME_LABEL = { system: "System", light: "Light", dark: "Dark" };
const THEME_ICON = { system: "monitor", light: "sun", dark: "moon" };

function storageGet(key) {
  try { return window.localStorage.getItem(key); } catch (e) { return null; }
}
function storageSet(key, value) {
  try { window.localStorage.setItem(key, value); } catch (e) { /* private mode: no persistence */ }
}

class App {
  constructor() {
    this.formats = new Map();
    this.formatList = [];
    this.settings = { max_upload_mb: 200, theme: "system" };
    this.cards = [];
    this.queueEl = document.getElementById("queue");
    this.uploadChain = Promise.resolve();
    this.history = new HistoryView(this);
    this.settingsDialog = new SettingsDialog(this);
  }

  async init() {
    document.getElementById("dz-icon").append(icon("upload", 42));
    document.getElementById("settings-icon").append(icon("gear", 18));
    this.applyTheme(document.documentElement.getAttribute("data-theme") || "system", false);
    this.bindTabs();
    this.bindDropzone();
    this.bindBatch();
    document.getElementById("theme-toggle").addEventListener("click", () => this.cycleTheme());
    try {
      const [health, settings, formats] = await Promise.all([api.health(), api.settings(), api.formats()]);
      document.getElementById("version").textContent = "v" + health.version;
      this.settings = settings;
      this.applyTheme(settings.theme, false);
      this.setFormats(formats);
    } catch (e) {
      this.fatal(friendlyError(e));
      return;
    }
    await this.history.load();
    await this.restore();
    this.renderQueue();
  }

  fatal(message) {
    clear(this.queueEl);
    this.queueEl.append(h("li", { class: "error-msg", role: "alert", text: message }));
  }

  setFormats(doc) {
    this.formatList = doc.formats;
    this.formats = new Map(doc.formats.map((f) => [f.id, f]));
    const readable = doc.formats.filter((f) => f.can_read).flatMap((f) => f.extensions);
    document.getElementById("file-input").setAttribute("accept", readable.join(","));
  }

  // ---------- theme ----------
  applyTheme(theme, persist = true) {
    if (!THEMES.includes(theme)) theme = "system";
    document.documentElement.setAttribute("data-theme", theme);
    storageSet("oc-theme", theme);
    const btn = document.getElementById("theme-toggle");
    btn.setAttribute("aria-label", "Theme: " + THEME_LABEL[theme] + ". Click to change.");
    document.getElementById("theme-label").textContent = THEME_LABEL[theme];
    const holder = document.getElementById("theme-icon");
    clear(holder).append(icon(THEME_ICON[theme], 18));
    if (persist) {
      this.settings.theme = theme;
      api.saveSettings({ theme }).catch((e) => announce("Could not save the theme: " + friendlyError(e)));
    }
  }

  cycleTheme() {
    const cur = document.documentElement.getAttribute("data-theme") || "system";
    const next = THEMES[(THEMES.indexOf(cur) + 1) % THEMES.length];
    this.applyTheme(next);
    announce("Theme: " + THEME_LABEL[next]);
  }

  async settingsChanged(saved) {
    this.settings = saved;
    this.applyTheme(saved.theme, false);
    this.setFormats(await api.formats());
    for (const c of this.cards) if (!c.busy) c.render(true);
  }

  // ---------- tabs ----------
  bindTabs() {
    const tabs = [document.getElementById("tab-convert"), document.getElementById("tab-history")];
    const select = (tab, focus) => {
      for (const t of tabs) {
        const on = t === tab;
        t.setAttribute("aria-selected", on ? "true" : "false");
        t.tabIndex = on ? 0 : -1;
        document.getElementById(t.getAttribute("aria-controls")).hidden = !on;
      }
      if (focus) tab.focus();
      if (tab.id === "tab-history") this.history.load();
    };
    tabs.forEach((t, i) => {
      t.addEventListener("click", () => select(t, false));
      t.addEventListener("keydown", (ev) => {
        if (ev.key === "ArrowRight" || ev.key === "ArrowLeft") {
          ev.preventDefault();
          select(tabs[(i + (ev.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length], true);
        }
      });
    });
    this.historyTab = tabs[1];
  }

  updateHistoryCount(n) {
    const tab = this.historyTab;
    let badge = tab.querySelector(".count");
    if (!badge) { badge = h("span", { class: "count" }); tab.append(badge); }
    badge.textContent = String(n);
    badge.hidden = n === 0;
  }

  historyChanged() {
    clearTimeout(this.historyTimer);
    this.historyTimer = setTimeout(() => this.history.load(), 300);
  }

  // ---------- adding files ----------
  bindDropzone() {
    const dz = document.getElementById("dropzone");
    const input = document.getElementById("file-input");
    dz.addEventListener("click", () => input.click());
    dz.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); input.click(); }
    });
    input.addEventListener("change", () => {
      this.addFiles(Array.from(input.files));
      input.value = "";
    });
    let depth = 0;
    const hasFiles = (ev) => ev.dataTransfer && Array.from(ev.dataTransfer.types || []).includes("Files");
    window.addEventListener("dragenter", (ev) => {
      if (!hasFiles(ev)) return;
      ev.preventDefault();
      depth++;
      document.body.classList.add("dragging");
    });
    window.addEventListener("dragover", (ev) => {
      if (!hasFiles(ev)) return;
      ev.preventDefault();
      ev.dataTransfer.dropEffect = "copy";
    });
    window.addEventListener("dragleave", () => {
      depth = Math.max(0, depth - 1);
      if (depth === 0) document.body.classList.remove("dragging");
    });
    window.addEventListener("drop", (ev) => {
      if (!hasFiles(ev)) return;
      ev.preventDefault();
      depth = 0;
      document.body.classList.remove("dragging");
      this.addFiles(Array.from(ev.dataTransfer.files));
    });
  }

  addFiles(files) {
    if (!files.length) return;
    for (const file of files) {
      const card = new Card(this, { name: file.name, size: file.size, file });
      this.cards.push(card);
      this.queueEl.append(card.el);
      // Upload one at a time so order is stable and memory stays low.
      this.uploadChain = this.uploadChain.then(() => card.upload());
    }
    this.renderQueue();
    announce(files.length === 1 ? "Uploading " + files[0].name : "Uploading " + files.length + " files");
  }

  defaultTarget(entry) {
    const t = entry.available_targets || [];
    const src = entry.detected_format;
    const prefs = { image: src === "png" ? ["jpeg"] : ["png"], data: src === "json" ? ["csv"] : ["json"], text: ["txt"] };
    const cat = this.formats.get(src) ? this.formats.get(src).category : "image";
    for (const p of prefs[cat] || []) if (t.includes(p)) return p;
    return t.find((x) => x !== src) || t[0];
  }

  // Estimate from this machine's own history (never an invented number).
  estimateSeconds(megapixels, target) {
    const rates = (fmt) => this.history.entries
      .filter((e) => e.status === "done" && e.megapixels > 0.5 && e.duration_ms > 0 && (!fmt || e.target_format === fmt))
      .map((e) => e.duration_ms / e.megapixels);
    let r = rates(target);
    if (r.length === 0) r = rates(null);
    if (r.length === 0) return null;
    r.sort((a, b) => a - b);
    const median = r[Math.floor(r.length / 2)];
    return { seconds: (median * megapixels) / 1000, samples: r.length };
  }

  // ---------- queue ----------
  renderQueue() {
    const existing = new Set(Array.from(this.queueEl.children));
    for (const c of this.cards) if (!existing.has(c.el)) this.queueEl.append(c.el);
    let empty = document.getElementById("queue-empty");
    if (this.cards.length === 0) {
      if (!empty) this.queueEl.append(h("li", { class: "empty", id: "queue-empty", text: "No files yet. Add some above to get started." }));
    } else if (empty) {
      empty.remove();
    }
    this.updateBatch();
  }

  removeCard(card) {
    this.cards = this.cards.filter((c) => c !== card);
    card.el.remove();
    this.save();
    this.renderQueue();
    announce(card.name + " removed.");
    const dz = document.getElementById("dropzone");
    if (!this.cards.length) dz.focus();
  }

  save() {
    storageSet(STORE_KEY, JSON.stringify(this.cards.filter((c) => c.entry || c.error).map((c) => c.toJSON())));
  }

  async restore() {
    let saved = [];
    try { saved = JSON.parse(storageGet(STORE_KEY) || "[]"); } catch (e) { saved = []; }
    let serverJobs = [];
    try { serverJobs = (await api.jobs()).jobs; } catch (e) { serverJobs = []; }
    const jobsById = new Map(serverJobs.map((j) => [j.job_id, j]));
    const histById = new Map(this.history.entries.map((e) => [e.id, e]));
    const knownJobs = new Set();
    const ids = saved.filter((s) => s && s.entry && s.entry.file_id).map((s) => s.entry.file_id);
    const outs = saved.filter((s) => s && s.result && s.result.output_id).map((s) => s.result.output_id);
    let existing = {};
    let outputsAlive = {};
    if (ids.length || outs.length) {
      try {
        const r = await api.lookup(ids, outs);
        existing = r.files;
        outputsAlive = r.outputs;
      } catch (e) { existing = {}; outputsAlive = {}; }
    }
    for (const s of saved) {
      if (!s || !s.name) continue;
      const card = new Card(this, s);
      if (card.jobId) knownJobs.add(card.jobId);
      if (card.jobId && jobsById.has(card.jobId)) {
        card.apply(jobsById.get(card.jobId));
        if (!["done", "failed", "cancelled"].includes(card.status)) card.track();
      } else if (card.jobId && ["queued", "running"].includes(card.status)) {
        const hist = histById.get(card.jobId);
        if (hist) {
          card.status = hist.status;
          if (hist.status === "done") {
            card.result = { output_id: hist.output_id, output_name: hist.output_name, size: hist.output_size,
              input_size: hist.input_size, duration_ms: hist.duration_ms, notes: hist.notes || [] };
          } else {
            card.error = { code: hist.error_code || "cancelled", message: "The conversion did not finish." };
          }
        } else {
          card.status = "failed";
          card.error = { code: "not_found", message: "The server restarted before this conversion finished." };
        }
      }
      if (card.status === "done" && card.result && !outputsAlive[card.result.output_id]) {
        card.status = "ready";
        card.result = null;
      }
      if (card.entry && card.status !== "error" && !existing[card.entry.file_id]) {
        // The upload expired (retention) or the data folder was reset.
        if (card.status !== "done") { card.status = "error"; card.error = { code: "not_found", message: "" }; }
        card.uploadGone = true;
      }
      card.render(true);
      this.cards.push(card);
    }
    // Jobs still running on the server that this page does not know about (another tab, a lost store).
    for (const j of serverJobs) {
      if (knownJobs.has(j.job_id) || ["done", "failed", "cancelled"].includes(j.state)) continue;
      try {
        const entry = await api.file(j.file_id);
        const card = new Card(this, { name: entry.original_name, size: entry.size, entry, target: j.target_format,
          jobId: j.job_id, status: j.state });
        card.apply(j);
        card.track();
        this.cards.push(card);
      } catch (e) { /* upload gone */ }
    }
    this.save();
  }

  // ---------- batch bar ----------
  bindBatch() {
    document.getElementById("set-all").addEventListener("change", (ev) => {
      const t = ev.target.value;
      if (!t) return;
      let n = 0;
      for (const c of this.cards) {
        if (c.entry && !c.busy && c.entry.available_targets.includes(t)) {
          c.target = t;
          c.options = {};
          if (["done", "failed", "cancelled"].includes(c.status)) c.status = "ready";
          c.render(true);
          n++;
        }
      }
      ev.target.value = "";
      this.save();
      this.updateBatch();
      const f = this.formats.get(t);
      announce("Set " + n + " file" + (n === 1 ? "" : "s") + " to " + (f ? f.name : t) + ".");
    });
    document.getElementById("convert-all").addEventListener("click", () => {
      for (const c of this.cards) if (c.entry && ["ready", "failed", "cancelled"].includes(c.status)) c.convert();
    });
    document.getElementById("download-all").addEventListener("click", () => this.downloadAll());
    document.getElementById("clear-finished").addEventListener("click", () => {
      for (const c of [...this.cards]) if (["done", "cancelled", "error"].includes(c.status)) c.remove();
    });
  }

  async downloadAll() {
    const done = this.cards.filter((c) => c.status === "done" && c.result);
    for (const c of done) {
      const a = h("a", { href: api.downloadUrl(c.result.output_id), download: c.result.output_name || "" });
      document.body.append(a);
      a.click();
      a.remove();
      await new Promise((r) => setTimeout(r, 400)); // sequential downloads (no zip in 1.0)
    }
  }

  updateBatch() {
    const bar = document.getElementById("batch");
    bar.hidden = this.cards.length === 0;
    const done = this.cards.filter((c) => c.status === "done").length;
    const busy = this.cards.filter((c) => c.busy).length;
    const failed = this.cards.filter((c) => c.status === "failed" || c.status === "error").length;
    const parts = [this.cards.length + " file" + (this.cards.length === 1 ? "" : "s")];
    if (busy) parts.push(busy + " in progress");
    if (done) parts.push(done + " done");
    if (failed) parts.push(failed + " failed");
    document.getElementById("batch-summary").textContent = parts.join(" · ");
    const sel = document.getElementById("set-all");
    const targets = new Set();
    for (const c of this.cards) if (c.entry) for (const t of c.entry.available_targets) targets.add(t);
    const current = Array.from(sel.options).slice(1).map((o) => o.value).join(",");
    const wanted = this.formatList.filter((f) => targets.has(f.id)).map((f) => f.id);
    if (current !== wanted.join(",")) {
      while (sel.options.length > 1) sel.remove(1);
      for (const id of wanted) sel.append(h("option", { value: id, text: this.formats.get(id).name }));
    }
    document.getElementById("download-all").disabled = done === 0;
    document.getElementById("convert-all").disabled = !this.cards.some((c) => c.entry && ["ready", "failed", "cancelled"].includes(c.status));
  }
}

const app = new App();
window.addEventListener("DOMContentLoaded", () => app.init());
