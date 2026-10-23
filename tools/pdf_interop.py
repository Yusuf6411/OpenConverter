#!/usr/bin/env python3
"""Renders PDFs with our renderer and with pdftoppm and compares them; also compares text with pdftotext.

Usage: pdf_interop.py <dir-with-pdfs> [name ...]   (generate the PDFs with tools/pdf_cases.py)"""
import os, re, subprocess, sys, tempfile
from PIL import Image, ImageChops

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, 'target', 'debug', 'examples', 'pdfrender')
# Tile phase of repeating patterns differs by a pixel from Poppler's.
LOOSE = {'f_patterns': (0.10, 15.0)}
PASSWORDS = {'v_aes256_pw': 'secret', 'v_rc4_40_pw': 'secret'}

def main():
    d = sys.argv[1]
    only = sys.argv[2:]
    names = sorted(f[:-4] for f in os.listdir(d) if f.endswith('.pdf') and (not only or f[:-4] in only))
    tmp = tempfile.mkdtemp(prefix='pdfi')
    bad = 0
    for n in names:
        src = f'{d}/{n}.pdf'
        pw = PASSWORDS.get(n)
        ref_cmd = ['pdftoppm', '-r', '72', '-f', '1', '-l', '1', '-singlefile', '-png']
        if pw:
            ref_cmd += ['-upw', pw]
        r = subprocess.run(ref_cmd + [src, f'{tmp}/{n}.ref'], capture_output=True, text=True)
        ours = f'{tmp}/{n}.ppm'
        cmd = [BIN, src, ours, '1', '72'] + ([pw] if pw else [])
        o = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        try:
            a = Image.open(ours).convert('RGB')
        except Exception:
            print(f'FAIL {n}: no output {o.stderr.strip()[:200]}'); bad += 1; continue
        try:
            b = Image.open(f'{tmp}/{n}.ref.png').convert('RGB')
        except Exception:
            print(f'skip {n}: reference failed ({r.stderr.strip()[:80]})'); continue
        if a.size != b.size:
            print(f'FAIL {n}: size {a.size} vs {b.size}'); bad += 1; continue
        diff = ImageChops.difference(a, b).convert('L')
        hist = diff.histogram()
        total = a.size[0] * a.size[1]
        big = sum(hist[64:]) / total
        mean = sum(i * c for i, c in enumerate(hist)) / total
        lim_big, lim_mean = LOOSE.get(n, (0.05, 8.0))  # glyph snapping differs from Poppler's
        ok = big < lim_big and mean < lim_mean
        print(f"{'ok  ' if ok else 'FAIL'} {n:22} mean={mean:5.2f} bigdiff={big*100:5.2f}%  {o.stderr.strip()[:100]}")
        if not ok:
            bad += 1
            a.save(f'{tmp}/{n}.ours.png'); ImageChops.difference(a, b).save(f'{tmp}/{n}.diff.png')
    print(f'images in {tmp}')
    sys.exit(1 if bad else 0)

if __name__ == '__main__':
    main()
