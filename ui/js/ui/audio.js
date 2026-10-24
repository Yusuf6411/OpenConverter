// Audio preview: waveform drawn from the decoded samples, loudness figures, tags and playback.
import { api } from "../api.js";
import { append, clear, h } from "./dom.js";
import { friendlyError } from "./format.js";

export function isAudio(fmt) {
  return !!fmt && fmt.category === "audio";
}

function clock(s) {
  const m = Math.floor(s / 60);
  const sec = s - m * 60;
  return m + ":" + (sec < 10 ? "0" : "") + sec.toFixed(1);
}

function lufs(v) { return v === null || v === undefined ? "–" : v.toFixed(1) + " LUFS"; }
function db(v) { return v === null || v === undefined || v < -200 ? "–" : v.toFixed(1) + " dBFS"; }

export class AudioDialog {
  // `target` is {id, output}: a stored upload or a finished output.
  constructor(app, name, target) {
    this.app = app;
    this.target = target;
    this.info = null;
    this.status = h("p", { class: "hint", role: "status", text: "Reading…" });
    this.canvas = h("canvas", { class: "waveform", width: 800, height: 140, role: "img",
      "aria-label": "Waveform of " + name });
    this.stats = h("dl", { class: "stats" });
    this.tags = h("div");
    this.player = h("audio", { controls: true, preload: "none", class: "player" });
    this.player.src = target.output ? api.outputAudioWavUrl(target.id) : api.fileAudioWavUrl(target.id);
    this.playhead = 0;
    const close = h("button", { type: "button", class: "btn primary" }, "Close");
    const body = h("div", { class: "dialog-body" }, h("h2", { id: "audio-title", text: name }));
    append(body, [this.status, this.canvas, this.player, this.stats, this.tags]);
    this.dialog = h("dialog", { "aria-labelledby": "audio-title", class: "wide" },
      h("form", { method: "dialog" }, body, h("div", { class: "dialog-actions" }, close)));
    close.addEventListener("click", () => this.dialog.close());
    this.dialog.addEventListener("close", () => { this.player.pause(); this.dialog.remove(); });
    this.player.addEventListener("timeupdate", () => { this.playhead = this.player.currentTime; this.draw(); });
    this.canvas.addEventListener("click", (e) => {
      if (!this.info || !this.info.duration) return;
      const r = this.canvas.getBoundingClientRect();
      this.player.currentTime = ((e.clientX - r.left) / r.width) * this.info.duration;
    });
    document.body.append(this.dialog);
    this.dialog.showModal();
    this.load();
  }

  async load() {
    try {
      this.info = await api.audioInfo(this.target.id, this.target.output, 800);
      this.status.textContent = this.info.channels + (this.info.channels === 1 ? " channel · " : " channels · ") +
        this.info.sample_rate + " Hz · " + clock(this.info.duration) +
        (this.info.float ? " · floating point" : this.info.bits ? " · " + this.info.bits + " bit" : "");
      this.draw();
      this.fill();
    } catch (e) {
      this.status.textContent = friendlyError(e);
      this.canvas.hidden = true;
      this.player.hidden = true;
    }
  }

  colour(name, fallback) {
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return v || fallback;
  }

  draw() {
    const info = this.info;
    if (!info) return;
    const c = this.canvas;
    const g = c.getContext("2d");
    const w = c.width, hgt = c.height, mid = hgt / 2;
    g.clearRect(0, 0, w, hgt);
    g.fillStyle = this.colour("--surface-2", "#f3f4f6");
    g.fillRect(0, 0, w, hgt);
    const n = info.peaks.length;
    const played = info.duration ? this.playhead / info.duration : 0;
    for (let i = 0; i < n; i++) {
      const [lo, hi] = info.peaks[i];
      const x = (i / n) * w;
      const y1 = mid - Math.min(1, hi) * mid * 0.95;
      const y2 = mid - Math.max(-1, lo) * mid * 0.95;
      g.fillStyle = i / n < played ? this.colour("--accent", "#2563eb") : this.colour("--text-2", "#6b7280");
      g.fillRect(x, y1, Math.max(1, w / n - 0.5), Math.max(1, y2 - y1));
    }
    g.fillStyle = this.colour("--border", "#d1d5db");
    g.fillRect(0, mid, w, 1);
  }

  fill() {
    const l = this.info.loudness;
    clear(this.stats);
    const rows = [
      ["Integrated loudness", lufs(l.integrated)],
      ["Loudness range", l.range === null ? "–" : l.range.toFixed(1) + " LU"],
      ["Sample peak", db(l.sample_peak_db)],
      ["True peak", db(l.true_peak_db)],
    ];
    for (const [k, v] of rows) append(this.stats, [h("dt", { text: k }), h("dd", { text: v })]);
    clear(this.tags);
    if (this.info.tags.length) {
      const dl = h("dl", { class: "stats" });
      for (const [k, v] of this.info.tags) append(dl, [h("dt", { text: k }), h("dd", { text: v })]);
      append(this.tags, [h("h3", { text: "Tags" }), dl]);
    }
  }
}
