// Tiny DOM helpers. User-controlled strings are only ever inserted as text
// (textContent / text nodes), never as HTML.

export function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v === undefined || v === null || v === false) continue;
      if (k === "class") el.className = v;
      else if (k === "text") el.textContent = v;
      else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
      else if (k === "dataset") Object.assign(el.dataset, v);
      else if (v === true) el.setAttribute(k, "");
      else el.setAttribute(k, String(v));
    }
  }
  append(el, children);
  return el;
}

export function append(el, children) {
  for (const c of children.flat()) {
    if (c === undefined || c === null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

export function clear(el) {
  while (el.firstChild) el.removeChild(el.firstChild);
  return el;
}

const SVG_NS = "http://www.w3.org/2000/svg";
const ICONS = {
  upload: ["M12 15V4", "M7 9l5-5 5 5", "M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3"],
  sun: ["M12 4V2", "M12 22v-2", "M4.9 4.9 3.5 3.5", "M20.5 20.5l-1.4-1.4", "M4 12H2", "M22 12h-2",
        "M4.9 19.1l-1.4 1.4", "M20.5 3.5l-1.4 1.4", "M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10z"],
  moon: ["M20 14.5A8.5 8.5 0 0 1 9.5 4 8.5 8.5 0 1 0 20 14.5z"],
  monitor: ["M3 5h18v11H3z", "M8 20h8", "M12 16v4"],
  gear: ["M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z",
         "M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"],
  image: ["M4 5h16v14H4z", "M4 16l5-5 4 4 2-2 5 5", "M15.5 9.5a1.5 1.5 0 1 0 0-.01"],
  text: ["M6 3h9l4 4v14H6z", "M14 3v5h5", "M9 13h7", "M9 17h7", "M9 9h2"],
  table: ["M4 5h16v14H4z", "M4 10h16", "M4 15h16", "M10 5v14"],
  audio: ["M9 18V6l10-2v12", "M9 18a3 3 0 1 1-3-3 3 3 0 0 1 3 3z", "M19 16a3 3 0 1 1-3-3 3 3 0 0 1 3 3z"],
  subtitle: ["M3 5h18v14H3z", "M7 12h4", "M13 12h4", "M7 15.5h2", "M11 15.5h6"],
  download: ["M12 4v11", "M7 10l5 5 5-5", "M5 20h14"],
  x: ["M6 6l12 12", "M18 6 6 18"],
  play: ["M7 5l12 7-12 7z"],
  stop: ["M7 7h10v10H7z"],
  retry: ["M4 12a8 8 0 1 0 2.3-5.6", "M4 4v4h4"],
  trash: ["M4 7h16", "M10 11v6", "M14 11v6", "M6 7l1 13h10l1-13", "M9 7V4h6v3"],
  alert: ["M12 3 2 20h20z", "M12 10v4", "M12 17v.01"],
};

export function icon(name, size = 18) {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.8");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  for (const d of ICONS[name] || []) {
    const p = document.createElementNS(SVG_NS, "path");
    p.setAttribute("d", d);
    svg.append(p);
  }
  return svg;
}

let liveTimer = null;
export function announce(message) {
  const live = document.getElementById("live");
  if (!live) return;
  live.textContent = "";
  clearTimeout(liveTimer);
  liveTimer = setTimeout(() => { live.textContent = message; }, 50);
}
