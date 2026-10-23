#!/usr/bin/env python3
"""Compare oc-font against fontTools (a test-time oracle only) on installed fonts."""
import glob, json, subprocess, sys
from fontTools.ttLib import TTFont
from fontTools.pens.areaPen import AreaPen
from fontTools.pens.recordingPen import RecordingPen

ROOT = __file__.rsplit("/tools/", 1)[0]
EXE = f"{ROOT}/target/debug/examples/fontdump"
fails, checks = [], 0

def check(name, ok, detail=""):
    global checks
    checks += 1
    if not ok:
        fails.append((name, detail))
        print("FAIL", name, detail)

files = sorted(glob.glob("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf")
               + glob.glob("/usr/share/fonts/truetype/liberation/LiberationSerif-Italic.ttf")
               + glob.glob("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
               + glob.glob("/usr/share/fonts/opentype/inter/Inter-Regular.otf")
               + glob.glob("/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf"))
for f in files:
    step = 7
    out = subprocess.run([EXE, f, str(step)], capture_output=True, text=True).stdout.splitlines()
    rows = [json.loads(l) for l in out]
    tt = TTFont(f)
    gs = tt.getGlyphSet()
    order = tt.getGlyphOrder()
    head = rows[0]
    check(f"{f} upem", head["upem"] == tt["head"].unitsPerEm)
    check(f"{f} glyph count", head["glyphs"] == len(order), (head["glyphs"], len(order)))
    bad = 0
    n = 0
    for r in rows[1:]:
        if "g" in r:
            name = order[r["g"]]
            pen = AreaPen(gs)
            gs[name].draw(pen)
            want = pen.value
            adv = tt["hmtx"][name][0]
            n += 1
            if r["adv"] != adv:
                bad += 1
                if bad < 4: print("  adv", f, name, r["adv"], adv)
            elif abs(r["area"] - want) > max(2.0, abs(want) * 0.003):
                bad += 1
                if bad < 4: print("  area", f, name, r["area"], want)
        else:
            pass
    check(f"{f} outlines+advances ({n} glyphs)", bad == 0, bad)
    cmap = tt.getBestCmap()
    wrong = [c for c in (r for r in rows[1:] if "c" in r) if cmap.get(c["c"]) is None or order.index(cmap[c["c"]]) != c["gid"]]
    check(f"{f} cmap", not wrong, wrong[:3])

from fontTools.encodings.StandardEncoding import StandardEncoding
from fontTools.cffLib import cffStandardStrings
enc = json.loads(subprocess.run([EXE, "--stdenc"], capture_output=True, text=True).stdout)
want = [cffStandardStrings.index(n) if n != ".notdef" else 0 for n in StandardEncoding]
check("standard encoding SIDs", enc == want, [i for i in range(256) if enc[i] != want[i]][:5])

# Conversions through the CLI, read back with fontTools and loaded by Chromium (OTS).
import os, tempfile
CLI = f"{ROOT}/target/debug/openconverter"
tmp = tempfile.mkdtemp(prefix="fontconv")
def conv(src, name):
    out = os.path.join(tmp, name)
    r = subprocess.run([CLI, "convert", src, out], capture_output=True, text=True)
    check(f"convert {os.path.basename(src)} -> {name}", r.returncode == 0, r.stderr[-200:])
    return out
LIB = "/usr/share/fonts/truetype/liberation/LiberationSerif-Italic.ttf"
INTER = "/usr/share/fonts/opentype/inter/Inter-Regular.otf"
outs = {"a.otf": (LIB, LIB), "a.woff": (LIB, LIB), "b.ttf": (INTER, INTER), "b.woff": (INTER, INTER)}
made = {n: conv(src, n) for n, (src, _) in outs.items()}
made["a2.ttf"] = conv(made["a.woff"], "a2.ttf")
made["b2.otf"] = conv(made["b.ttf"], "b2.otf")
refs = {"a.otf": LIB, "a.woff": LIB, "a2.ttf": LIB, "b.ttf": INTER, "b.woff": INTER, "b2.otf": INTER}
for name, path in made.items():
    f = TTFont(path); r = TTFont(refs[name])
    gs, rs = f.getGlyphSet(), r.getGlyphSet()
    fo, ro = f.getGlyphOrder(), r.getGlyphOrder()
    bad = 0
    for i in range(0, len(ro), 9):
        p1 = AreaPen(gs); gs[fo[i]].draw(p1)
        p2 = AreaPen(rs); rs[ro[i]].draw(p2)
        if abs(p1.value - p2.value) > max(3, abs(p2.value) * 0.006) or f["hmtx"][fo[i]][0] != r["hmtx"][ro[i]][0]:
            bad += 1
    check(f"{name} glyphs match source", len(fo) == len(ro) and bad == 0, (len(fo), len(ro), bad))
    ids = {c: f.getGlyphID(n) for c, n in f.getBestCmap().items()}
    rids = {c: r.getGlyphID(n) for c, n in r.getBestCmap().items()}
    check(f"{name} cmap keeps glyph ids", ids == rids)
res = subprocess.run(["node", f"{ROOT}/tools/font_ots.js", *made.values()], capture_output=True, text=True,
                     env={**os.environ, "NODE_PATH": "/opt/node-tools/node_modules"})
for line in res.stdout.splitlines():
    check("browser loads " + os.path.basename(line.split()[0]), line.split()[1:2] == ["ok"], line)
check("specimen picture", subprocess.run([CLI, "convert", LIB, os.path.join(tmp, "s.png")], capture_output=True).returncode == 0)

print(f"{checks - len(fails)}/{checks} checks passed")
sys.exit(1 if fails else 0)
