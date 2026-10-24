// Table preview: the first rows of a spreadsheet or delimited file, with a sheet picker.
import { api } from "../api.js";
import { append, clear, h } from "./dom.js";
import { friendlyError } from "./format.js";

export function isPreviewableTable(fmt) {
  return !!fmt && fmt.category === "data";
}

export class TableDialog {
  constructor(app, card) {
    this.app = app;
    this.card = card;
    this.sheet = "";
    this.status = h("p", { class: "hint", role: "status" });
    this.picker = h("select", { "aria-label": "Sheet" });
    this.picker.addEventListener("change", () => { this.sheet = this.picker.value; this.load(); });
    this.pickerRow = h("div", { class: "field-row" }, h("label", { text: "Sheet" }), this.picker);
    this.pickerRow.hidden = true;
    this.grid = h("div", { class: "table-scroll", tabindex: "0", role: "region", "aria-label": "Table preview" });
    const body = h("div", { class: "dialog-body" }, h("h2", { id: "table-title", text: card.name }));
    const close = h("button", { type: "button", class: "btn primary" }, "Close");
    const actions = h("div", { class: "dialog-actions" }, close);
    this.dialog = h("dialog", { "aria-labelledby": "table-title", class: "wide" },
      h("form", { method: "dialog" }, body, actions));
    close.addEventListener("click", () => this.dialog.close());
    this.dialog.addEventListener("close", () => this.dialog.remove());
    append(body, [this.status, this.pickerRow, this.grid]);
    document.body.append(this.dialog);
    this.dialog.showModal();
    this.load();
  }

  async load() {
    this.status.textContent = "Reading…";
    clear(this.grid);
    try {
      const doc = await api.tablePreview(this.card.entry.file_id, this.sheet, 100);
      this.render(doc);
    } catch (e) {
      this.status.textContent = friendlyError(e);
    }
  }

  render(doc) {
    if (doc.sheets.length > 1 && this.pickerRow.hidden) {
      for (const s of doc.sheets) {
        this.picker.append(h("option", { value: s.name, text: s.name + (s.hidden ? " (hidden)" : "") }));
      }
      this.pickerRow.hidden = false;
    }
    if (doc.sheet !== undefined) this.picker.value = doc.sheet;
    this.status.textContent = doc.total_rows + " row" + (doc.total_rows === 1 ? "" : "s") + ", " +
      doc.total_cols + " column" + (doc.total_cols === 1 ? "" : "s") + (doc.truncated ? " · showing the first " + doc.rows.length : "");
    const table = h("table", { class: "preview-table" });
    const head = h("tr");
    for (const c of doc.header) head.append(h("th", { scope: "col", text: c }));
    table.append(h("thead", null, head));
    const tbody = h("tbody");
    for (const r of doc.rows) {
      const tr = h("tr");
      for (const c of r) tr.append(h("td", { text: c }));
      tbody.append(tr);
    }
    table.append(tbody);
    this.grid.append(table);
  }
}
