// Build an options panel from the server's option schemas (/api/formats).
import { h } from "./dom.js";

let uid = 0;

// Which target formats flatten transparency (they show the background picker).
export function flattensAlpha(fmt) {
  return fmt && !!fmt.write_options.background;
}

export function renderOptions(container, writeSchema, readSchema, values, onChange, { showBackground = true, disabled = false } = {}) {
  while (container.firstChild) container.removeChild(container.firstChild);
  addFields(container, writeSchema, values, onChange, showBackground);
  if (readSchema && Object.keys(readSchema).length) {
    const inner = h("div", { class: "options nested" });
    addFields(inner, readSchema, values, onChange, showBackground);
    container.append(h("details", { class: "opt-more" }, h("summary", { text: "Input options" }), inner));
  }
  for (const el of container.querySelectorAll("input, select")) el.disabled = disabled;
}

function addFields(container, schema, values, onChange, showBackground) {
  for (const [key, opt] of Object.entries(schema)) {
    if (key === "background" && !showBackground) continue;
    const id = "opt-" + (++uid);
    const value = key in values ? values[key] : opt.default;
    const help = opt.help ? opt.help : undefined;
    if (opt.type === "int" && opt.max - opt.min <= 100 && key === "quality") {
      const out = h("output", { for: id, text: String(value) });
      const input = h("input", { type: "range", id, min: opt.min, max: opt.max, step: 1, value, title: help,
        "aria-valuetext": String(value) });
      input.addEventListener("input", () => {
        out.textContent = input.value;
        input.setAttribute("aria-valuetext", input.value);
        onChange(key, Number(input.value));
      });
      container.append(h("div", { class: "opt" }, h("label", { for: id, text: opt.label }),
        h("div", { class: "range-row" }, input, out)));
    } else if (opt.type === "int") {
      const input = h("input", { type: "number", id, min: opt.min, max: opt.max, step: 1, value, title: help });
      input.addEventListener("change", () => {
        let v = Math.round(Number(input.value));
        if (!Number.isFinite(v)) v = opt.default;
        v = Math.min(opt.max, Math.max(opt.min, v));
        input.value = v;
        onChange(key, v);
      });
      container.append(h("div", { class: "opt" }, h("label", { for: id, text: opt.label + " (" + opt.min + "–" + opt.max + ")" }), input));
    } else if (opt.type === "bool") {
      const input = h("input", { type: "checkbox", id, title: help });
      input.checked = !!value;
      input.addEventListener("change", () => onChange(key, input.checked));
      container.append(h("div", { class: "opt inline" }, input, h("label", { for: id, text: opt.label })));
    } else if (opt.type === "choice") {
      const sel = h("select", { id, title: help });
      for (const c of opt.choices) {
        const label = opt.choice_labels && opt.choice_labels[c] !== undefined ? opt.choice_labels[c] : String(c);
        const o = h("option", { value: String(c), text: label });
        if (c === value) o.selected = true;
        sel.append(o);
      }
      sel.addEventListener("change", () => {
        const raw = sel.value;
        const v = typeof opt.choices[0] === "number" ? Number(raw) : raw;
        onChange(key, v);
      });
      container.append(h("div", { class: "opt" }, h("label", { for: id, text: opt.label }), sel));
    } else if (opt.type === "colour") {
      const input = h("input", { type: "color", id, value: String(value).toLowerCase(), title: help });
      input.addEventListener("change", () => onChange(key, input.value.toUpperCase()));
      container.append(h("div", { class: "opt inline" }, input, h("label", { for: id, text: opt.label })));
    }
  }
}
