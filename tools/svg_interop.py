#!/usr/bin/env python3
"""Renders tools/svg_cases.py documents with our renderer and with Chromium and compares them."""
import os, subprocess, sys, tempfile, json
sys.path.insert(0, os.path.dirname(__file__))
from svg_cases import CASES
from PIL import Image, ImageChops

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, 'target', 'debug', 'examples', 'svgrender')
NODE_PATH = '/opt/node-tools/node_modules:' + os.environ.get('NODE_PATH', '')
SHOT = r'''
const { chromium } = require('playwright-core');
const fs = require('fs');
(async () => {
  const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome', args: ['--no-sandbox'] });
  const dir = process.argv[2];
  for (const f of fs.readdirSync(dir).filter(f => f.endsWith('.svg'))) {
    const svg = fs.readFileSync(dir + '/' + f, 'utf8');
    const p = await b.newPage({ viewport: { width: 400, height: 400 } });
    await p.setContent('<html><body style="margin:0;background:#fff">' + svg + '</body></html>');
    const el = await p.$('svg');
    await el.screenshot({ path: dir + '/' + f.replace('.svg', '.chrome.png') });
    await p.close();
  }
  await b.close();
})();
'''
# Glyph anti-aliasing and hinting differ from Chromium's, so text gets a looser bound.
TEXTY = {'text'}

def main():
    only = sys.argv[1:]
    d = tempfile.mkdtemp(prefix='svgi')
    names = [n for n in CASES if not only or n in only]
    for n in names:
        open(f'{d}/{n}.svg', 'w').write(CASES[n])
    open(f'{d}/shot.js', 'w').write(SHOT)
    shot_dir = d
    env = dict(os.environ, NODE_PATH=NODE_PATH)
    subprocess.run(['node', f'{d}/shot.js', shot_dir], check=True, env=env)
    bad = 0
    for n in names:
        ppm = f'{d}/{n}.ppm'
        r = subprocess.run([BIN, f'{d}/{n}.svg', ppm], capture_output=True, text=True)
        try:
            ours = Image.open(ppm).convert('RGB')
        except Exception:
            print(f'FAIL {n}: no output {r.stderr.strip()[:200]}'); bad += 1; continue
        ref = Image.open(f'{d}/{n}.chrome.png').convert('RGB')
        if ours.size != ref.size:
            print(f'FAIL {n}: size {ours.size} vs {ref.size}'); bad += 1; continue
        diff = ImageChops.difference(ours, ref).convert('L')
        hist = diff.histogram()
        total = ours.size[0] * ours.size[1]
        big = sum(hist[48:]) / total
        mean = sum(i * c for i, c in enumerate(hist)) / total
        lim_big, lim_mean = (0.06, 8.0) if n in TEXTY else (0.02, 3.0)
        ok = big < lim_big and mean < lim_mean
        print(f"{'ok  ' if ok else 'FAIL'} {n:14} mean={mean:5.2f} bigdiff={big*100:5.2f}%  {r.stderr.strip()[:90]}")
        if not ok:
            bad += 1
            ours.save(f'{d}/{n}.ours.png')
            Image.merge('RGB', (diff, diff, diff)).save(f'{d}/{n}.diff.png')
    print(f'{len(names)-bad}/{len(names)} cases match; files in {d}')
    sys.exit(1 if bad else 0)
main()
