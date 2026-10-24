#!/usr/bin/env python3
"""Translation catalog tooling.

  tools/i18n.py extract   mark plain-text elements in ui/index.html with data-i18n keys and
                          write ui/locales/en.json (keys already present are kept)
  tools/i18n.py pseudo    write ui/locales/qps.json, the pseudo-locale, from en.json
  tools/i18n.py check     every locale has exactly the keys of en.json and the same {placeholders}
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HTML = ROOT / "ui" / "index.html"
LOC = ROOT / "ui" / "locales"

ACCENT = str.maketrans(
    "AEIOUaeiouCcNnYySsZz",
    "ÀÉÎÕÜàéîõüÇçÑñÝýŠšŽž",
)


def pseudo(s: str) -> str:
    """Accented, ~35% longer text; {placeholders} are left alone."""
    out, i = [], 0
    for m in re.finditer(r"\{[a-z_]+\}", s):
        out.append(s[i:m.start()].translate(ACCENT))
        out.append(m.group(0))
        i = m.end()
    out.append(s[i:].translate(ACCENT))
    text = "".join(out)
    pad = "~" * max(2, len(re.sub(r"\{[a-z_]+\}", "", s)) // 3)
    return f"[{text}{pad}]"


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", text.lower()).strip("_")[:40]


def extract() -> None:
    html = HTML.read_text()
    cat = {}
    en = LOC / "en.json"
    if en.exists():
        cat = json.loads(en.read_text())
    leaf = re.compile(r"<(?P<tag>h1|h2|button|label|option|th|p|span|footer|a)(?P<attrs>[^>]*)>(?P<text>[^<>]*[A-Za-z][^<>]*)</(?P=tag)>")

    def sub(m):
        attrs, text = m.group("attrs"), m.group("text")
        if "data-i18n" in attrs or "aria-hidden" in attrs or text.strip().startswith("v…"):
            return m.group(0)
        idm = re.search(r'\bid="([^"]+)"', attrs)
        key = (idm.group(1).replace("-", "_") if idm else slug(text)).strip()
        base, n = key, 2
        while key in cat and cat[key] != text.strip():
            key, n = f"{base}_{n}", n + 1
        cat[key] = text.strip()
        return f'<{m.group("tag")}{attrs} data-i18n="{key}">{text}</{m.group("tag")}>'

    html = leaf.sub(sub, html)
    HTML.write_text(html)
    LOC.mkdir(exist_ok=True)
    en.write_text(json.dumps(cat, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(f"extract: {len(cat)} strings")


def write_pseudo() -> None:
    en = json.loads((LOC / "en.json").read_text())
    (LOC / "qps.json").write_text(
        json.dumps({k: pseudo(v) for k, v in en.items()}, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    )
    print(f"pseudo: {len(en)} strings")


def check() -> int:
    en = json.loads((LOC / "en.json").read_text())
    bad = 0
    html_keys = set(re.findall(r'data-i18n="([^"]+)"', HTML.read_text()))
    for k in sorted(html_keys - set(en)):
        print(f"index.html uses {k!r} which en.json lacks")
        bad += 1
    for f in sorted(LOC.glob("*.json")):
        if f.name == "en.json":
            continue
        loc = json.loads(f.read_text())
        for k in sorted(set(en) - set(loc)):
            print(f"{f.name}: missing {k}")
            bad += 1
        for k in sorted(set(loc) - set(en)):
            print(f"{f.name}: extra {k}")
            bad += 1
        for k in set(en) & set(loc):
            if sorted(re.findall(r"\{[a-z_]+\}", en[k])) != sorted(re.findall(r"\{[a-z_]+\}", loc[k])):
                print(f"{f.name}: placeholders differ in {k}")
                bad += 1
    print("check: ok" if not bad else f"check: {bad} problems")
    return 1 if bad else 0


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "extract":
        extract()
    elif cmd == "pseudo":
        write_pseudo()
    elif cmd == "check":
        sys.exit(check())
    else:
        print(__doc__)
        sys.exit(2)
