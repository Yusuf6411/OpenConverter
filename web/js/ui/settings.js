// Settings dialog (native <dialog>), saved through PUT /api/settings.
import { api } from "../api.js";
import { announce } from "./dom.js";
import { friendlyError } from "./format.js";

const FIELDS = {
  theme: "set-theme",
  default_jpeg_quality: "set-quality",
  default_png_level: "set-level",
  alpha_background: "set-bg",
  retention_hours: "set-retention",
  max_upload_mb: "set-maxup",
};

export class SettingsDialog {
  constructor(app) {
    this.app = app;
    this.dialog = document.getElementById("settings-dialog");
    this.form = document.getElementById("settings-form");
    this.opener = document.getElementById("settings-open");
    this.errorBox = document.getElementById("settings-error");
    this.opener.addEventListener("click", () => this.open());
    document.getElementById("settings-cancel").addEventListener("click", () => this.dialog.close());
    this.form.addEventListener("submit", (ev) => { ev.preventDefault(); this.save(); });
    this.dialog.addEventListener("close", () => this.opener.focus());
  }

  open() {
    const s = this.app.settings;
    for (const [key, id] of Object.entries(FIELDS)) {
      const el = document.getElementById(id);
      el.value = key === "alpha_background" ? String(s[key]).toLowerCase() : String(s[key]);
      this.setFieldError(key, "");
    }
    this.errorBox.textContent = "";
    this.dialog.showModal();
    document.getElementById(FIELDS.theme).focus();
  }

  setFieldError(key, msg) {
    const box = document.getElementById("err-" + key);
    if (box) box.textContent = msg;
    const el = document.getElementById(FIELDS[key]);
    if (el) el.setAttribute("aria-invalid", msg ? "true" : "false");
  }

  collect() {
    const out = {};
    let ok = true;
    for (const [key, id] of Object.entries(FIELDS)) {
      const el = document.getElementById(id);
      if (el.type === "number") {
        const v = Number(el.value);
        if (!el.value || !Number.isInteger(v) || v < Number(el.min) || v > Number(el.max)) {
          this.setFieldError(key, "Enter a whole number from " + el.min + " to " + el.max + ".");
          ok = false;
          continue;
        }
        this.setFieldError(key, "");
        out[key] = v;
      } else if (key === "alpha_background") {
        out[key] = el.value.toUpperCase();
      } else {
        out[key] = el.value;
      }
    }
    return ok ? out : null;
  }

  async save() {
    this.errorBox.textContent = "";
    const values = this.collect();
    if (!values) {
      this.errorBox.textContent = "Please fix the highlighted fields.";
      return;
    }
    try {
      const saved = await api.saveSettings(values);
      await this.app.settingsChanged(saved);
      this.dialog.close();
      announce("Settings saved.");
    } catch (e) {
      this.errorBox.textContent = friendlyError(e);
    }
  }
}
