#!/usr/bin/env python3
"""Compares our PDF text extraction with pdftotext: word-level F1 and order similarity."""
import os, subprocess, sys, re, difflib
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, 'target', 'debug', 'examples', 'pdftext')
PASSWORDS = {'v_aes256_pw': 'secret', 'v_rc4_40_pw': 'secret'}

def words(t):
    return re.findall(r'\S+', t)

def f1(a, b):
    from collections import Counter
    ca, cb = Counter(a), Counter(b)
    common = sum((ca & cb).values())
    if not a and not b: return 1.0
    if not common: return 0.0
    p, r = common / len(a), common / len(b)
    return 2 * p * r / (p + r)

def main():
    d = sys.argv[1]
    only = sys.argv[2:]
    bad = 0
    for f in sorted(os.listdir(d)):
        if not f.endswith('.pdf') or (only and f[:-4] not in only): continue
        n = f[:-4]
        pw = PASSWORDS.get(n)
        ref = subprocess.run(['pdftotext'] + (['-upw', pw] if pw else []) + [f'{d}/{f}', '-'], capture_output=True, text=True).stdout
        ours = subprocess.run([BIN, f'{d}/{f}', 'txt'] + ([pw] if pw else []), capture_output=True, text=True, timeout=120).stdout
        a, b = words(ours), words(ref)
        score = f1(a, b)
        order = difflib.SequenceMatcher(None, a, b, autojunk=False).ratio() if len(a) < 4000 else score
        ok = score > 0.9 and order > 0.9 or (not b and not a)
        print(f"{'ok  ' if ok else 'FAIL'} {n:22} f1={score:.3f} order={order:.3f} words={len(a)}/{len(b)}")
        bad += not ok
    sys.exit(1 if bad else 0)

main()
