// One file in the conversion queue.
import { api, ApiError, uploadFile } from "../api.js";
import { announce, append, clear, h, icon } from "./dom.js";
import { ArchiveDialog, isOpenableArchive } from "./archive.js";
import { TableDialog, isPreviewableTable } from "./table.js";
import { AudioDialog, isAudio } from "./audio.js";
import { VideoDialog, isVideo } from "./video.js";
import { AUDIO_PRESETS, VIDEO_PRESETS, presetOptions } from "./presets.js";
import { bytes, duration, friendlyError, sizeChange, stageLabel } from "./format.js";
import { flattensAlpha, renderOptions } from "./options.js";

const FINAL = new Set(["done", "failed", "cancelled"]);
let nextLocal = 1;

export class Card {
  constructor(app, init) {
    this.app = app;
    this.localId = init.localId || "c" + Date.now().toString(36) + (nextLocal++);
    this.name = init.name;
    this.size = init.size;
    this.file = init.file || null;
    this.entry = init.entry || null;       // POST /api/files result
    this.target = init.target || null;
    this.options = init.options || {};     // only values the user changed
    this.jobId = init.jobId || null;
    this.status = init.status || "uploading";
    this.progress = init.progress || 0;
    this.stage = init.stage || "";
    this.result = init.result || null;
    this.error = init.error || null;
    this.uploadFraction = 0;
    this.es = null;
    this.pollTimer = null;
    this.sseRetryTimer = null;
    this.build();
  }

  toJSON() {
    return { localId: this.localId, name: this.name, size: this.size, entry: this.entry, target: this.target,
      options: this.options, jobId: this.jobId, status: this.status === "uploading" ? "error" : this.status,
      progress: this.progress, stage: this.stage, result: this.result,
      error: this.status === "uploading" ? { code: "network", message: "The upload was interrupted by a page reload." } : this.error };
  }

  get format() { return this.entry ? this.app.formats.get(this.entry.detected_format) : null; }
  get targetFormat() { return this.target ? this.app.formats.get(this.target) : null; }
  get busy() { return this.status === "queued" || this.status === "running" || this.status === "uploading"; }
  get canConvert() { return this.entry && ["ready", "failed", "cancelled", "done"].includes(this.status); }

  // ---------- DOM ----------
  build() {
    this.el = h("li", { class: "card", "aria-label": this.name });
    this.thumb = h("div", { class: "thumb" });
    this.main = h("div", { class: "card-main" });
    this.actions = h("div", { class: "card-actions" });
    append(this.el, [this.thumb, this.main, this.actions]);

    this.titleName = h("span", { class: "name", text: this.name, title: this.name });
    this.titleBadges = h("span", { class: "meta" });
    this.metaRow = h("div", { class: "meta" });
    this.warnRow = h("div");
    this.convertRow = h("div", { class: "convert-row" });
    this.optionsBox = h("div", { class: "options" });
    this.hintRow = h("div");
    this.bar = h("span");
    this.barWrap = h("div", { class: "bar", role: "progressbar", "aria-valuemin": "0", "aria-valuemax": "100",
      "aria-valuenow": "0", "aria-label": "Progress for " + this.name }, this.bar);
    this.pct = h("span", { class: "pct", text: "" });
    this.progressRow = h("div", { class: "progress" }, this.barWrap, this.pct);
    this.resultRow = h("div");
    append(this.main, [h("div", { class: "card-title" }, this.titleName, this.titleBadges), this.metaRow,
      this.warnRow, this.convertRow, this.optionsBox, this.hintRow, this.progressRow, this.resultRow]);

    this.targetSelect = h("select", { "aria-label": "Convert " + this.name + " to" });
    this.targetSelect.addEventListener("change", () => {
      this.target = this.targetSelect.value;
      this.options = {};
      if (FINAL.has(this.status)) this.status = "ready";
      this.render(true);
      this.app.save();
    });
    this.convertBtn = h("button", { type: "button", class: "btn primary" }, icon("play", 16), "Convert");
    this.convertBtn.addEventListener("click", () => this.convert());
    this.cancelBtn = h("button", { type: "button", class: "btn" }, icon("stop", 16), "Cancel");
    this.cancelBtn.addEventListener("click", () => this.cancel());
    this.downloadLink = h("a", { class: "btn", role: "button" }, icon("download", 16), "Download");
    this.browseBtn = h("button", { type: "button", class: "btn" }, icon("text", 16), "Browse");
    this.browseBtn.addEventListener("click", () => new ArchiveDialog(this.app, this));
    this.previewBtn = h("button", { type: "button", class: "btn" }, icon("text", 16), "Preview");
    this.previewBtn.addEventListener("click", () => new TableDialog(this.app, this));
    this.waveBtn = h("button", { type: "button", class: "btn" }, icon("audio", 16), "Waveform");
    this.waveBtn.addEventListener("click", () => new AudioDialog(this.app, this.name, { id: this.entry.file_id, output: false }));
    this.outWaveBtn = h("button", { type: "button", class: "btn" }, icon("audio", 16), "Output waveform");
    this.outWaveBtn.addEventListener("click", () => new AudioDialog(this.app, this.result.output_name || this.name, { id: this.result.output_id, output: true }));
    this.streamsBtn = h("button", { type: "button", class: "btn" }, icon("video", 16), "Streams and trim");
    this.streamsBtn.addEventListener("click", () => new VideoDialog(this.app, this));
    this.presetSelect = h("select", { "aria-label": "Preset for " + this.name });
    this.presetSelect.addEventListener("change", () => this.applyPreset(this.presetSelect.value));
    this.removeBtn = h("button", { type: "button", class: "btn", "aria-label": "Remove " + this.name }, icon("x", 16), "Remove");
    this.removeBtn.addEventListener("click", () => this.remove());
    this.render(true);
  }

  setThumb() {
    clear(this.thumb);
    const fmt = this.format;
    const hasPreview = this.result && this.status === "done" ? !this.outputGone : !this.uploadGone;
    if (this.entry && fmt && fmt.category === "image" && hasPreview) {
      const src = this.result && this.status === "done" ? api.outputPreviewUrl(this.result.output_id)
        : api.filePreviewUrl(this.entry.file_id);
      const img = h("img", { alt: "", src, loading: "lazy", decoding: "async" });
      img.addEventListener("error", () => { clear(this.thumb); this.thumb.append(icon("image", 36)); });
      this.thumb.append(img);
    } else {
      const kind = fmt ? (fmt.category === "data" ? "table" : fmt.category === "text" ? "text" : fmt.category === "audio" ? "audio" : fmt.category === "video" ? "video" : fmt.category === "subtitle" ? "subtitle" : fmt.category === "font" ? "font" : "image") : "upload";
      this.thumb.append(icon(kind, 36));
    }
  }

  render(full = false) {
    // Re-rendering detaches and re-attaches controls; keep keyboard focus on them.
    const active = document.activeElement;
    const hadFocus = active && active !== document.body && this.el.contains(active);
    this.renderInner(full);
    if (hadFocus && document.activeElement !== active) {
      if (active.isConnected && !active.disabled) active.focus();
      else {
        const fallback = this.actions.querySelector("button, a");
        if (fallback) fallback.focus();
      }
    }
  }

  renderInner(full) {
    const fmt = this.format;
    const tfmt = this.targetFormat;
    this.el.className = "card state-" + this.status;
    if (full) this.setThumb();

    // badges + meta
    clear(this.titleBadges);
    if (fmt) this.titleBadges.append(h("span", { class: "badge fmt", text: fmt.name, title: "Detected from the file's content" }));
    clear(this.metaRow);
    const meta = [bytes(this.size)];
    if (this.entry && this.entry.width) meta.push(this.entry.width + " × " + this.entry.height + " px");
    for (const m of meta) this.metaRow.append(h("span", { text: m }));
    if (fmt && tfmt) {
      const own = [fmt, tfmt].every((f) => f.ownership === "OWN");
      const partial = [fmt, tfmt].filter((f) => f.ownership !== "OWN").map((f) => f.name + ": " + (f.ownership_note || f.ownership));
      this.metaRow.append(h("span", { class: "badge " + (own ? "own" : "partial"),
        text: own ? "Own codecs" : "Partly stdlib",
        title: own ? "Both formats are handled by OpenConverter's own code." : partial.join(" ") }));
    }

    clear(this.warnRow);
    if (this.entry && this.entry.extension_mismatch && fmt) {
      this.warnRow.append(h("div", { class: "warning" }, icon("alert", 14),
        " The file name says otherwise, but the content is " + fmt.name + "."));
    }

    // target + options (only when we know what the file is)
    clear(this.convertRow);
    if (this.entry && this.entry.available_targets && this.entry.available_targets.length) {
      if (!this.target || !this.entry.available_targets.includes(this.target)) this.target = this.app.defaultTarget(this.entry);
      if (full || this.targetSelect.options.length === 0) {
        clear(this.targetSelect);
        for (const t of this.entry.available_targets) {
          const f = this.app.formats.get(t);
          const o = h("option", { value: t, text: (f ? f.name : t) + (t === this.entry.detected_format ? " (same format)" : "") });
          if (t === this.target) o.selected = true;
          this.targetSelect.append(o);
        }
      }
      this.targetSelect.disabled = this.busy;
      for (const el of this.optionsBox.querySelectorAll("input, select")) el.disabled = this.busy;
      append(this.convertRow, [h("label", { class: "hint", text: "Convert to" }), this.targetSelect]);
      if (isAudio(fmt)) {
        if (full || this.presetSelect.options.length === 0) {
          clear(this.presetSelect);
          this.presetSelect.append(h("option", { value: "", text: "Preset…" }));
          for (const p of AUDIO_PRESETS) {
            if (this.entry.available_targets.includes(p.target)) this.presetSelect.append(h("option", { value: p.id, text: p.name }));
          }
        }
        this.presetSelect.disabled = this.busy;
        append(this.convertRow, [this.presetSelect]);
      }
      if (isVideo(fmt)) {
        if (full || this.presetSelect.options.length === 0) {
          clear(this.presetSelect);
          this.presetSelect.append(h("option", { value: "", text: "Preset…" }));
          for (const p of VIDEO_PRESETS) {
            if (this.entry.available_targets.includes(p.target)) this.presetSelect.append(h("option", { value: p.id, text: p.name }));
          }
        }
        this.presetSelect.disabled = this.busy;
        append(this.convertRow, [this.presetSelect]);
      }
      if (full) this.renderOptions();
    } else {
      clear(this.optionsBox);
    }

    // large image hint
    clear(this.hintRow);
    const mp = this.entry && this.entry.width ? (this.entry.width * this.entry.height) / 1e6 : 0;
    if (mp > 4 && !FINAL.has(this.status)) {
      const est = this.app.estimateSeconds(mp, this.target);
      const text = est
        ? "Large image (" + mp.toFixed(1) + " MP): estimated time may be long, about " + Math.round(est.seconds) +
          " s based on " + est.samples + " earlier conversion" + (est.samples === 1 ? "" : "s") + " on this computer."
        : "Large image (" + mp.toFixed(1) + " MP): estimated time may be long (no timing history on this computer yet).";
      this.hintRow.append(h("div", { class: "hint", text }));
    }

    // progress
    const showProgress = this.status !== "ready" && this.status !== "error";
    this.progressRow.hidden = !showProgress;
    let pct = this.status === "uploading" ? this.uploadFraction * 100 : this.progress;
    if (this.status === "done") pct = 100;
    pct = Math.max(0, Math.min(100, pct));
    this.bar.style.setProperty("--p", pct.toFixed(1) + "%");
    this.barWrap.setAttribute("aria-valuenow", String(Math.round(pct)));
    const label = this.status === "uploading" ? "Uploading" : stageLabel(this.status === "running" ? this.stage : this.status);
    this.barWrap.setAttribute("aria-valuetext", label + " " + Math.round(pct) + "%");
    this.pct.textContent = Math.round(pct) + "% " + label;

    // result / error
    clear(this.resultRow);
    if (this.status === "done" && this.result) {
      const r = this.result;
      const change = sizeChange(r.input_size || this.size, r.size);
      const row = h("div", { class: "result" },
        h("span", {}, "Output ", h("strong", { text: bytes(r.size) })),
        change === null ? null : h("span", { class: change <= 0 ? "smaller" : "larger",
          text: (change > 0 ? "+" : "") + change.toFixed(1) + "%" }),
        h("span", { text: "in " + duration(r.duration_ms) }),
        h("span", { class: "hint", text: r.output_name }));
      this.resultRow.append(row);
      if (r.notes && r.notes.length) {
        const ul = h("ul", { class: "notes", "aria-label": "Notes" });
        for (const n of r.notes) ul.append(h("li", { text: n }));
        this.resultRow.append(ul);
      }
    } else if ((this.status === "failed" || this.status === "error") && this.error) {
      this.resultRow.append(h("div", { class: "error-msg", role: "status", text: friendlyError(this.error) }));
    } else if (this.status === "cancelled") {
      this.resultRow.append(h("div", { class: "hint", text: "Cancelled. No output was kept." }));
    }

    // actions
    clear(this.actions);
    if (this.busy && this.status !== "uploading") this.actions.append(this.cancelBtn);
    if (this.canConvert) {
      this.convertBtn.lastChild.textContent = this.status === "failed" || this.status === "cancelled" ? "Retry"
        : this.status === "done" ? "Convert again" : "Convert";
      this.actions.append(this.convertBtn);
    }
    if (this.status === "done" && this.result) {
      this.downloadLink.href = api.downloadUrl(this.result.output_id);
      this.downloadLink.setAttribute("download", this.result.output_name || "");
      this.actions.append(this.downloadLink);
    }
    if (this.entry && isOpenableArchive(this.format) && !this.uploadGone) this.actions.append(this.browseBtn);
    if (this.entry && isPreviewableTable(this.format) && !this.uploadGone) this.actions.append(this.previewBtn);
    if (this.entry && isAudio(this.format) && !this.uploadGone) this.actions.append(this.waveBtn);
    if (this.entry && isVideo(this.format) && !this.uploadGone) this.actions.append(this.streamsBtn);
    if (this.status === "done" && this.result && isAudio(this.targetFormat) && !this.outputGone) this.actions.append(this.outWaveBtn);
    this.actions.append(this.removeBtn);
  }

  applyPreset(id) {
    const p = [...AUDIO_PRESETS, ...VIDEO_PRESETS].find((x) => x.id === id);
    this.presetSelect.value = "";
    if (!p || !this.entry || !this.entry.available_targets.includes(p.target)) return;
    const tgt = this.app.formats.get(p.target);
    this.target = p.target;
    this.options = presetOptions(p, tgt ? tgt.write_options : null);
    if (FINAL.has(this.status)) this.status = "ready";
    // make the target menu show the new choice
    for (const o of this.targetSelect.options) o.selected = o.value === p.target;
    this.render(true);
    this.app.save();
    announce("Preset applied: " + p.name + ".");
  }

  renderOptions() {
    const src = this.format;
    const tgt = this.targetFormat;
    if (!src || !tgt) return;
    const showBg = flattensAlpha(tgt) && !!(this.entry && this.entry.may_have_alpha);
    renderOptions(this.optionsBox, tgt.write_options, src.read_options, this.options, (k, v) => {
      this.options[k] = v;
      this.app.save();
    }, { showBackground: showBg, disabled: this.busy });
  }

  // ---------- actions ----------
  async upload() {
    const limit = this.app.settings.max_upload_mb * 1024 * 1024;
    if (this.file.size > limit) {
      this.fail("error", { code: "limit_exceeded", message: "this file is " + bytes(this.file.size) +
        "; the limit is " + this.app.settings.max_upload_mb + " MB (change it in Settings)." });
      return;
    }
    this.status = "uploading";
    this.render();
    try {
      const entry = await uploadFile(this.file, (f) => { this.uploadFraction = f; this.render(); });
      this.file = null;
      if (entry.error) {
        this.fail("error", entry.error);
        return;
      }
      this.entry = entry;
      this.status = "ready";
      this.target = this.app.defaultTarget(entry);
      this.render(true);
      announce(this.name + " added as " + (this.format ? this.format.name : entry.detected_format) + ".");
    } catch (e) {
      this.fail("error", e instanceof ApiError ? { code: e.code, message: e.message } : { code: "network", message: String(e) });
    }
    this.app.save();
    this.app.updateBatch();
  }

  fail(status, error) {
    this.status = status;
    this.error = error;
    this.render(true);
    this.app.save();
    this.app.updateBatch();
    announce(this.name + ": " + friendlyError(error));
  }

  async convert() {
    if (!this.canConvert) return;
    this.stopTracking();
    this.status = "queued";
    this.cancelRequested = false;
    this.progress = 0;
    this.stage = "queued";
    this.result = null;
    this.error = null;
    this.render(true);
    try {
      const { job_id } = await api.createJob(this.entry.file_id, this.target, this.options);
      this.jobId = job_id;
      this.app.save();
      this.track();
    } catch (e) {
      this.fail("failed", { code: e.code, message: e.message });
    }
    this.app.updateBatch();
  }

  async cancel() {
    if (!this.jobId) return;
    this.cancelRequested = true;  // shown as "Cancelling" until the worker stops
    try {
      const snap = await api.cancel(this.jobId);
      this.apply(snap);
      if (!FINAL.has(this.status)) this.render();
    } catch (e) {
      announce("Could not cancel: " + friendlyError(e));
    }
  }

  remove() {
    if (this.busy && this.jobId) api.cancel(this.jobId).catch(() => {});
    this.stopTracking();
    this.app.removeCard(this);
  }

  // ---------- progress tracking: SSE with polling fallback ----------
  track() {
    this.stopTracking();
    if (!this.jobId) return;
    if (typeof EventSource === "undefined") { this.poll(); return; }
    const es = new EventSource(api.eventsUrl(this.jobId));
    this.es = es;
    const onEvent = (ev) => {
      try { this.apply(JSON.parse(ev.data)); } catch (e) { /* ignore a malformed event */ }
    };
    es.addEventListener("progress", onEvent);
    es.addEventListener("state", onEvent);
    es.addEventListener("error", () => {
      // Stream dropped (server busy or restarted): fall back to polling and
      // try the stream again a little later.
      es.close();
      if (this.es === es) this.es = null;
      if (!FINAL.has(this.status)) {
        this.poll();
        clearTimeout(this.sseRetryTimer);
        this.sseRetryTimer = setTimeout(() => { if (!FINAL.has(this.status)) this.track(); }, 10000);
      }
    });
  }

  poll() {
    clearTimeout(this.pollTimer);
    const tick = async () => {
      if (!this.jobId || FINAL.has(this.status)) return;
      try {
        this.apply(await api.job(this.jobId));
      } catch (e) {
        if (e.code === "not_found") {
          this.fail("failed", { code: "not_found", message: "The server no longer knows this job (it may have restarted)." });
          return;
        }
      }
      if (!FINAL.has(this.status) && !this.es) this.pollTimer = setTimeout(tick, 500);
    };
    tick();
  }

  stopTracking() {
    if (this.es) { this.es.close(); this.es = null; }
    clearTimeout(this.pollTimer);
    clearTimeout(this.sseRetryTimer);
  }

  apply(snap) {
    if (!snap || snap.job_id !== this.jobId) return;
    const wasFinal = FINAL.has(this.status);
    this.status = snap.state;
    this.progress = Math.max(this.progress, snap.progress || 0);
    this.stage = this.cancelRequested && !FINAL.has(snap.state) ? "cancelling" : snap.stage;
    if (snap.state === "done") this.result = snap.result;
    if (snap.state === "failed" || snap.state === "cancelled") this.error = snap.error;
    const nowFinal = FINAL.has(this.status);
    this.render(nowFinal && !wasFinal);
    if (nowFinal && !wasFinal) {
      this.stopTracking();
      this.app.save();
      this.app.updateBatch();
      this.app.historyChanged();
      if (snap.state === "done") announce(this.name + " converted to " + (this.targetFormat ? this.targetFormat.name : this.target) + ".");
      else if (snap.state === "failed") announce(this.name + " failed: " + friendlyError(this.error));
      else announce(this.name + ": conversion cancelled.");
    }
  }
}
