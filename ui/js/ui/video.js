// Video inspector: thumbnail strip, the streams of the file with a keep/drop choice for each,
// and trim marks. "Use these settings" writes them into the card's options.
import { api } from "../api.js";
import { append, clear, h, announce } from "./dom.js";
import { friendlyError, bytes } from "./format.js";

export function isVideo(fmt) {
  return !!fmt && fmt.category === "video" && fmt.id !== "frames.zip";
}

function clock(s) {
  const m = Math.floor(s / 60);
  const sec = s - m * 60;
  return m + ":" + (sec < 10 ? "0" : "") + sec.toFixed(1);
}

function describe(s) {
  if (s.kind === "video") {
    const fps = s.fps ? " · " + (Math.round(s.fps * 100) / 100) + " fps" : "";
    return s.width + "×" + s.height + fps;
  }
  if (s.kind === "audio") return s.channels + (s.channels === 1 ? " channel" : " channels") + " · " + s.sample_rate + " Hz";
  return "text";
}

export class VideoDialog {
  constructor(app, card) {
    this.app = app;
    this.card = card;
    this.info = null;
    this.keep = new Set();
    this.start = 0;
    this.end = 0;
    this.status = h("p", { class: "hint", role: "status", text: "Reading…" });
    this.strip = h("div", { class: "strip", role: "group", "aria-label": "Thumbnails of " + card.name });
    this.range = h("div", { class: "trim" });
    this.table = h("table", { class: "streams" });
    const apply = h("button", { type: "button", class: "btn primary" }, "Use these settings");
    const close = h("button", { type: "button", class: "btn" }, "Close");
    const body = h("div", { class: "dialog-body" }, h("h2", { id: "video-title", text: card.name }));
    append(body, [this.status, this.strip, this.range, this.table]);
    this.dialog = h("dialog", { "aria-labelledby": "video-title", class: "wide" },
      h("form", { method: "dialog" }, body, h("div", { class: "dialog-actions" }, close, apply)));
    close.addEventListener("click", () => this.dialog.close());
    apply.addEventListener("click", () => { this.apply(); this.dialog.close(); });
    this.dialog.addEventListener("close", () => this.dialog.remove());
    document.body.append(this.dialog);
    this.dialog.showModal();
    this.load();
  }

  async load() {
    try {
      this.info = await api.videoInfo(this.card.entry.file_id, 12);
      const i = this.info;
      this.status.textContent = i.container.toUpperCase() + " · " + clock(i.duration) +
        (i.bitrate_kbps ? " · " + Math.round(i.bitrate_kbps) + " kbit/s" : "") +
        " · " + i.streams.length + (i.streams.length === 1 ? " stream" : " streams");
      const o = this.card.options;
      const kept = (o.streams || "").split(",").map((x) => x.trim()).filter(Boolean).map(Number);
      this.keep = new Set(kept.length ? kept : i.streams.map((s) => s.index));
      this.start = (o.start_ms || 0) / 1000;
      this.end = o.duration_ms ? this.start + o.duration_ms / 1000 : i.duration;
      this.drawStrip();
      this.drawRange();
      this.drawStreams();
    } catch (e) {
      this.status.textContent = friendlyError(e);
    }
  }

  drawStrip() {
    clear(this.strip);
    const t = this.info.thumbs;
    if (!t.items.length) {
      this.strip.append(h("p", { class: "hint", text: t.note || "No thumbnails." }));
      return;
    }
    for (const it of t.items) {
      const img = h("img", { src: it.png, alt: "Frame at " + clock(it.t), width: t.width, height: t.height });
      const b = h("button", { type: "button", class: "thumb-btn", title: "Click: start here · Shift-click: end here" }, img,
        h("span", { text: clock(it.t) }));
      b.addEventListener("click", (e) => {
        if (e.shiftKey) this.end = Math.max(it.t, this.start + 0.1);
        else this.start = Math.min(it.t, this.end - 0.1);
        this.drawRange();
      });
      this.strip.append(b);
    }
  }

  drawRange() {
    const d = this.info.duration || 0;
    clear(this.range);
    if (!d) return;
    const num = (label, value, set) => {
      const id = "trim-" + label;
      const input = h("input", { type: "number", id, min: 0, max: d, step: 0.1, value: value.toFixed(1) });
      input.addEventListener("change", () => { set(Math.min(d, Math.max(0, Number(input.value) || 0))); this.drawRange(); });
      return h("div", { class: "opt" }, h("label", { for: id, text: label }), input);
    };
    append(this.range, [
      num("Start (s)", this.start, (v) => { this.start = Math.min(v, this.end - 0.1); }),
      num("End (s)", this.end, (v) => { this.end = Math.max(v, this.start + 0.1); }),
      h("p", { class: "hint", text: "Keeping " + clock(this.end - this.start) + " of " + clock(d) }),
    ]);
  }

  drawStreams() {
    clear(this.table);
    append(this.table, [h("thead", {}, h("tr", {}, ...["Keep", "#", "Type", "Codec", "Details", "Size", "How it is written"].map((t) => h("th", { scope: "col", text: t }))))]);
    const body = h("tbody");
    const target = this.card.target;
    for (const s of this.info.streams) {
      const box = h("input", { type: "checkbox", "aria-label": "Keep stream " + s.index });
      box.checked = this.keep.has(s.index);
      box.addEventListener("change", () => { if (box.checked) this.keep.add(s.index); else this.keep.delete(s.index); });
      let how;
      if (s.copy_to.includes(target)) how = "copied as it is";
      else if (s.kind === "subtitle") how = "text, rewritten if needed";
      else if (s.decodable) how = "re-encoded";
      else how = "cannot go into " + (target || "this format");
      body.append(h("tr", {}, h("td", {}, box), h("td", { text: String(s.index) }), h("td", { text: s.kind }),
        h("td", { text: s.codec + (s.language && s.language !== "und" ? " (" + s.language + ")" : "") }),
        h("td", { text: describe(s) }), h("td", { text: bytes(s.bytes) }), h("td", { text: how })));
    }
    this.table.append(body);
  }

  apply() {
    const o = this.card.options;
    const all = this.info.streams.map((s) => s.index);
    if (all.every((i) => this.keep.has(i))) delete o.streams;
    else o.streams = all.filter((i) => this.keep.has(i)).join(",");
    const d = this.info.duration;
    if (this.start > 0.05) o.start_ms = Math.round(this.start * 1000); else delete o.start_ms;
    if (this.end < d - 0.05) o.duration_ms = Math.round((this.end - this.start) * 1000); else delete o.duration_ms;
    this.card.render(true);
    this.app.save();
    announce("Video settings applied.");
  }
}
