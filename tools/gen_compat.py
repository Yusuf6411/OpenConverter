#!/usr/bin/env python3
"""Writes docs/COMPATIBILITY.md from a running `openconverter serve`.
Usage: tools/gen_compat.py http://127.0.0.1:PORT > docs/COMPATIBILITY.md"""
import json, sys, urllib.request

base = sys.argv[1].rstrip("/")
d = json.load(urllib.request.urlopen(base + "/api/formats"))
cats = {c["id"]: c["name"] for c in d["categories"]}
print("# Format compatibility\n")
print("Generated from the engine's own registry (`tools/gen_compat.py`). Every format listed is implemented by OpenConverter's own code.\n")
for cid, cname in cats.items():
    fs = [f for f in d["formats"] if f["category"] == cid]
    if not fs:
        continue
    print(f"## {cname}\n")
    print("| Format | Extensions | Read | Write | Converts to | Options |")
    print("| --- | --- | --- | --- | --- | --- |")
    for f in fs:
        yn = lambda b: "yes" if b else "no"
        opts = ", ".join(f"`{k}`" for k in f["write_options"]) or "none"
        tg = ", ".join(f"`{t}`" for t in f["targets"]) or "none"
        print(f"| {f['name']} (`{f['id']}`) | {' '.join(f['extensions'])} | {yn(f['can_read'])} | {yn(f['can_write'])} | {tg} | {opts} |")
    print()
