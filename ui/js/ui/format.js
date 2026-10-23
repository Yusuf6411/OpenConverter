// Formatting helpers and friendly error messages.

export function bytes(n) {
  if (n === null || n === undefined) return "–";
  if (n < 1024) return n + " B";
  const units = ["KB", "MB", "GB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return (v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2)) + " " + units[i];
}

export function duration(ms) {
  if (ms === null || ms === undefined) return "–";
  if (ms < 1000) return ms + " ms";
  const s = ms / 1000;
  if (s < 60) return s.toFixed(s < 10 ? 2 : 1) + " s";
  return Math.floor(s / 60) + " min " + Math.round(s % 60) + " s";
}

export function time(ts) {
  if (!ts) return "–";
  const d = new Date(ts * 1000);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  return sameDay ? d.toLocaleTimeString() : d.toLocaleString();
}

export function sizeChange(input, output) {
  if (!input || output === undefined || output === null) return null;
  return ((output - input) / input) * 100;
}

const STAGES = {
  queued: "Waiting…", decoding: "Decoding", converting: "Converting", encoding: "Encoding",
  finalizing: "Finishing", done: "Done", failed: "Failed", cancelled: "Cancelled",
  uploading: "Uploading", cancelling: "Cancelling…", inflating: "Decoding", compressing: "Encoding", parsing: "Reading", writing: "Writing",
};
export function stageLabel(stage) { return STAGES[stage] || stage || ""; }

// Friendly, actionable messages keyed on the API error code.
export function friendlyError(err) {
  const code = err && err.code;
  const msg = (err && err.message) || "";
  if (code === "unsupported_variant") {
    if (/progressive/i.test(msg)) return "This JPEG uses progressive encoding, which OpenConverter 1.0 doesn't support yet.";
    if (/arithmetic/i.test(msg)) return "This JPEG uses arithmetic coding, which OpenConverter 1.0 doesn't support.";
    if (/CMYK|4-component/i.test(msg)) return "This is a CMYK JPEG, which OpenConverter 1.0 doesn't support yet.";
    if (/cannot be represented/i.test(msg)) return msg;
    return "This file uses a feature OpenConverter 1.0 doesn't support: " + msg;
  }
  if (code === "corrupt_file") return "The file looks damaged or incomplete, so it couldn't be read. (" + msg + ")";
  if (code === "codec_not_in_build") return msg || "This file needs a codec that is not included in this build.";
  if (code === "unsupported_format") return msg || "This kind of file isn't supported yet.";
  if (code === "limit_exceeded") return "Too large: " + msg;
  if (code === "cancelled") return "Conversion cancelled.";
  if (code === "network") return msg;
  if (code === "not_found") return "The file is no longer available on the server (it may have expired). Please add it again.";
  if (code === "bad_request") return "Invalid setting: " + msg;
  if (code === "forbidden") return "The request was blocked by a security check. Reload the page and try again.";
  return msg || "Something went wrong.";
}
