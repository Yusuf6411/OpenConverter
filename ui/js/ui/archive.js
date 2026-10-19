// Archive tools: browse and extract an uploaded archive, and pack several files into one.
import { api } from "../api.js";
import { announce, append, clear, h } from "./dom.js";
import { bytes, friendlyError, time } from "./format.js";

export function isOpenableArchive(fmt) {
  return !!fmt && fmt.category === "archive" && fmt.id !== "file";
}

function dialogShell(title, labelId) {
  const body = h("div", { class: "dialog-body" });
  const actions = h("div", { class: "dialog-actions" });
  const form = h("form", { method: "dialog", novalidate: true }, body, actions);
  const dialog = h("dialog", { "aria-labelledby": labelId, class: "wide" }, form);
  body.append(h("h2", { id: labelId, text: title }));
  document.body.append(dialog);
  dialog.addEventListener("close", () => dialog.remove());
  return { dialog, body, actions, form };
}

function closeButton(dialog, label = "Close") {
  return h("button", { type: "button", class: "btn", onclick: () => dialog.close() }, label);
}

// ---------- browse / extract ----------
export class ArchiveDialog {
  constructor(app, card) {
    this.app = app;
    this.card = card;
    this.password = "";
    this.selected = new Set();
    this.entries = [];
    const s = dialogShell(card.name, "archive-title");
    this.dialog = s.dialog;
    this.body = s.body;
    this.status = h("p", { class: "hint", role: "status" });
    this.listBox = h("div", { class: "entry-list", role: "group", "aria-label": "Archive contents" });
    this.resultBox = h("div");
    this.extractBtn = h("button", { type: "submit", class: "btn primary", disabled: true }, "Extract");
    this.allBtn = h("button", { type: "button", class: "btn", disabled: true }, "Select all");
    this.allBtn.addEventListener("click", () => this.toggleAll());
    s.form.addEventListener("submit", (ev) => { ev.preventDefault(); this.extract(); });
    append(s.actions, [this.allBtn, closeButton(this.dialog), this.extractBtn]);
    append(this.body, [this.status, this.listBox, this.resultBox]);
    this.dialog.showModal();
    this.load();
  }

  async load() {
    clear(this.listBox);
    this.status.textContent = "Reading…";
    try {
      const doc = await api.archiveEntries(this.card.entry.file_id, this.password);
      this.entries = doc.entries;
      this.renderList(doc);
    } catch (e) {
      if (e.code === "encrypted_need_password" || e.code === "wrong_password") this.askPassword(e);
      else this.status.textContent = friendlyError(e);
    }
  }

  askPassword(e) {
    this.status.textContent = e.code === "wrong_password" ? "That password does not match." : "This archive is encrypted.";
    const input = h("input", { type: "password", id: "arc-pw", autocomplete: "off", "aria-label": "Archive password" });
    const go = h("button", { type: "button", class: "btn primary", text: "Unlock" });
    const submit = () => { this.password = input.value; this.load(); };
    go.addEventListener("click", submit);
    input.addEventListener("keydown", (ev) => { if (ev.key === "Enter") { ev.preventDefault(); submit(); } });
    append(clear(this.listBox), [h("div", { class: "field-row" }, h("label", { for: "arc-pw", text: "Password" }), input, go)]);
    input.focus();
  }

  renderList(doc) {
    this.status.textContent = doc.count + " item" + (doc.count === 1 ? "" : "s") + ", " + bytes(doc.total_size) +
      " unpacked" + (doc.encrypted ? " · encrypted" : "") + " · " + doc.format;
    clear(this.listBox);
    this.boxes = [];
    const sorted = [...doc.entries].sort((a, b) => a.path.localeCompare(b.path));
    for (const e of sorted) {
      const depth = e.path.replace(/\/$/, "").split("/").length - 1;
      const name = e.path.replace(/\/$/, "").split("/").pop();
      const box = h("input", { type: "checkbox", "aria-label": "Select " + e.path });
      box.addEventListener("change", () => {
        if (box.checked) this.selected.add(e.path); else this.selected.delete(e.path);
        this.updateButtons();
      });
      this.boxes.push([e.path, box]);
      const kind = e.kind === "dir" ? "📁" : e.kind === "link" ? "🔗" : "📄";
      const row = h("label", { class: "entry" }, box,
        h("span", { class: "entry-icon", "aria-hidden": "true", text: kind }),
        h("span", { class: "entry-name", text: name + (e.kind === "link" && e.link ? " → " + e.link : ""), title: e.path }),
        h("span", { class: "entry-size", text: e.kind === "file" ? bytes(e.size) : "" }),
        h("span", { class: "entry-time", text: e.mtime ? time(e.mtime) : "" }),
        e.encrypted ? h("span", { class: "badge", text: "encrypted" }) : null);
      row.style.paddingLeft = 8 + depth * 18 + "px";
      this.listBox.append(row);
    }
    this.allBtn.disabled = sorted.length === 0;
    this.updateButtons();
  }

  toggleAll() {
    const everything = this.selected.size < this.entries.length;
    this.selected = new Set(everything ? this.entries.map((e) => e.path) : []);
    for (const [p, box] of this.boxes) box.checked = this.selected.has(p);
    this.updateButtons();
  }

  updateButtons() {
    const n = this.selected.size;
    this.extractBtn.disabled = this.entries.length === 0;
    this.extractBtn.textContent = n ? "Extract " + n + " selected" : "Extract all";
    this.allBtn.textContent = n < this.entries.length ? "Select all" : "Select none";
  }

  async extract() {
    clear(this.resultBox);
    this.extractBtn.disabled = true;
    this.status.textContent = "Extracting…";
    try {
      const paths = [...this.selected].map((p) => p.replace(/\/$/, ""));
      const r = await api.archiveExtract(this.card.entry.file_id, paths, this.password);
      this.status.textContent = "Extracted " + r.files + " file" + (r.files === 1 ? "" : "s") + ".";
      const link = h("a", { class: "btn primary", role: "button", href: api.downloadUrl(r.output_id), download: r.output_name },
        "Download " + r.output_name + " (" + bytes(r.size) + ")");
      this.resultBox.append(link);
      if (r.skipped && r.skipped.length) {
        const ul = h("ul", { class: "notes", "aria-label": "Skipped" });
        for (const s of r.skipped) ul.append(h("li", { text: s.path + ": " + s.reason }));
        this.resultBox.append(h("p", { class: "hint", text: "Some items were left out for safety:" }), ul);
      }
      announce("Extraction finished.");
    } catch (e) {
      this.status.textContent = friendlyError(e);
    }
    this.updateButtons();
  }
}

// ---------- pack ----------
export class PackDialog {
  constructor(app) {
    this.app = app;
    const cards = app.cards.filter((c) => c.entry && !c.busy);
    const s = dialogShell("Pack into an archive", "pack-title");
    this.dialog = s.dialog;
    const formats = app.formatList.filter((f) => f.category === "archive" && f.id !== "file" && f.can_write);
    this.format = h("select", { id: "pack-format", "aria-label": "Archive format" });
    for (const f of formats) this.format.append(h("option", { value: f.id, text: f.name }));
    if (formats.some((f) => f.id === "zip")) this.format.value = "zip";
    this.name = h("input", { type: "text", id: "pack-name", placeholder: "archive", "aria-label": "Archive name" });
    this.password = h("input", { type: "password", id: "pack-pw", autocomplete: "new-password", "aria-label": "Password (optional)" });
    this.pwRow = h("div", { class: "field" }, h("label", { for: "pack-pw", text: "Password (optional)" }), this.password);
    this.format.addEventListener("change", () => this.syncPassword());
    this.status = h("p", { class: "hint", role: "status" });
    this.resultBox = h("div");
    this.packBtn = h("button", { type: "submit", class: "btn primary", disabled: cards.length === 0 }, "Create archive");
    this.cards = cards;
    append(s.body, [
      h("p", { class: "hint", text: cards.length + " file" + (cards.length === 1 ? "" : "s") + " will be packed under their own names." }),
      h("div", { class: "field" }, h("label", { for: "pack-format", text: "Format" }), this.format),
      h("div", { class: "field" }, h("label", { for: "pack-name", text: "Name" }), this.name),
      this.pwRow, this.status, this.resultBox,
    ]);
    append(s.actions, [closeButton(this.dialog), this.packBtn]);
    s.form.addEventListener("submit", (ev) => { ev.preventDefault(); this.pack(); });
    this.syncPassword();
    this.dialog.showModal();
  }

  syncPassword() {
    const f = this.app.formats.get(this.format.value);
    this.pwRow.hidden = !(f && f.write_options && Object.prototype.hasOwnProperty.call(f.write_options, "password"));
  }

  async pack() {
    clear(this.resultBox);
    this.packBtn.disabled = true;
    this.status.textContent = "Packing…";
    const options = {};
    if (!this.pwRow.hidden && this.password.value) options.password = this.password.value;
    const body = { file_ids: this.cards.map((c) => c.entry.file_id), target_format: this.format.value, options };
    if (this.name.value.trim()) body.name = this.name.value.trim();
    try {
      const { job_id } = await api.createArchive(body);
      for (;;) {
        const snap = await api.job(job_id);
        if (snap.state === "done") {
          const r = snap.result;
          const stem = this.name.value.trim().replace(/\.[^.]*$/, "");
          this.status.textContent = "Done: " + bytes(r.size) + ".";
          this.resultBox.append(h("a", { class: "btn primary", role: "button", href: api.downloadUrl(r.output_id),
            download: stem ? stem + "." + r.output_name.split(".").slice(1).join(".") : r.output_name }, "Download " + r.output_name));
          this.app.historyChanged();
          announce("Archive created.");
          break;
        }
        if (snap.state === "failed" || snap.state === "cancelled") {
          this.status.textContent = friendlyError(snap.error || {});
          break;
        }
        await new Promise((r) => setTimeout(r, 300));
      }
    } catch (e) {
      this.status.textContent = friendlyError(e);
    }
    this.packBtn.disabled = false;
  }
}
