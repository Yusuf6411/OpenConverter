// History tab: table of past conversions from /api/history.
import { api } from "../api.js";
import { announce, clear, h, icon } from "./dom.js";
import { bytes, duration, friendlyError, time } from "./format.js";

export class HistoryView {
  constructor(app) {
    this.app = app;
    this.body = document.getElementById("history-body");
    this.empty = document.getElementById("history-empty");
    this.entries = [];
    document.getElementById("history-refresh").addEventListener("click", () => this.load());
    document.getElementById("history-clear").addEventListener("click", () => this.confirmClear());
  }

  async load() {
    try {
      const { entries } = await api.history();
      this.entries = entries;
    } catch (e) {
      announce("Could not load history: " + friendlyError(e));
    }
    this.render();
    return this.entries;
  }

  render() {
    clear(this.body);
    this.empty.hidden = this.entries.length > 0;
    for (const e of this.entries) {
      const fmtName = (id) => { const f = this.app.formats.get(id); return f ? f.name : id; };
      const status = e.status === "done" && e.expired ? "expired" : e.status;
      const statusText = { done: "Done", failed: "Failed", cancelled: "Cancelled", expired: "Expired" }[status] || status;
      const actions = h("div", { class: "row-actions" });
      if (e.status === "done" && !e.expired && e.output_id) {
        actions.append(h("a", { class: "btn small", href: api.downloadUrl(e.output_id), download: e.output_name || "",
          "aria-label": "Download " + (e.output_name || "output") }, icon("download", 14), "Re-download"));
      }
      const del = h("button", { type: "button", class: "btn small", "aria-label": "Delete history entry for " + (e.original_name || "file") },
        icon("trash", 14), "Delete");
      del.addEventListener("click", async () => {
        try {
          await api.deleteHistory(e.id);
          this.entries = this.entries.filter((x) => x.id !== e.id);
          this.render();
          announce("History entry deleted.");
          document.getElementById("history-refresh").focus();
        } catch (err) {
          announce("Could not delete: " + friendlyError(err));
        }
      });
      actions.append(del);
      const sizeText = e.output_size !== null && e.output_size !== undefined
        ? bytes(e.input_size) + " → " + bytes(e.output_size) : bytes(e.input_size);
      this.body.append(h("tr", {},
        h("td", { class: "name", title: e.original_name || "", text: e.original_name || "–" }),
        h("td", { text: fmtName(e.source_format) + " → " + fmtName(e.target_format) }),
        h("td", { text: sizeText }),
        h("td", { text: duration(e.duration_ms) }),
        h("td", {}, h("span", { class: "status " + status, text: statusText, title: e.error_code || "" })),
        h("td", { text: time(e.timestamp) }),
        h("td", {}, actions)));
    }
    this.app.updateHistoryCount(this.entries.length);
  }

  confirmClear() {
    const dlg = document.getElementById("confirm-dialog");
    const opener = document.getElementById("history-clear");
    dlg.returnValue = "";
    dlg.showModal();
    dlg.addEventListener("close", async () => {
      opener.focus();
      if (dlg.returnValue !== "ok") return;
      try {
        await api.clearHistory();
        this.entries = [];
        this.render();
        announce("History cleared.");
      } catch (e) {
        announce("Could not clear history: " + friendlyError(e));
      }
    }, { once: true });
  }
}
