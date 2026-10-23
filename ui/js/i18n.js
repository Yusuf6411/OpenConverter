// Interface language. Catalogs are ui/locales/<lang>.json (key -> text, {name} placeholders).
// The language comes from ?lang=, then the saved choice, then the browser. "qps" is the
// pseudo-locale used to find strings that are not translatable yet. English is built into the
// page, so a missing catalog or key falls back to it.
let catalog = {};

function storageGet(key) {
  try { return window.localStorage.getItem(key); } catch (e) { return null; }
}

export function pickLanguage() {
  const q = new URLSearchParams(window.location.search).get("lang");
  const want = q || storageGet("oc-lang") || (navigator.language || "en");
  const tag = want.toLowerCase();
  return /^[a-z]{2,3}(-[a-z0-9]{2,8})*$/.test(tag) ? tag : "en";
}

export function t(key, fallback, vars) {
  let text = Object.prototype.hasOwnProperty.call(catalog, key) ? catalog[key] : fallback;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) text = text.split("{" + k + "}").join(String(v));
  }
  return text;
}

export async function loadLanguage() {
  const tag = pickLanguage();
  document.documentElement.setAttribute("lang", tag === "qps" ? "en" : tag);
  const names = [tag, tag.split("-")[0]].filter((n, i, a) => n !== "en" && a.indexOf(n) === i);
  for (const name of names) {
    try {
      const r = await fetch("/locales/" + name + ".json", { headers: { Accept: "application/json" } });
      if (r.ok) {
        catalog = await r.json();
        break;
      }
    } catch (e) { /* offline or missing: English */ }
  }
  for (const el of document.querySelectorAll("[data-i18n]")) {
    const key = el.getAttribute("data-i18n");
    if (Object.prototype.hasOwnProperty.call(catalog, key)) el.textContent = catalog[key];
  }
  return tag;
}
