#!/usr/bin/env python3
"""Writes small synthetic DNG files with a known scene, so the reader can be checked against
what the scene should look like. Needs numpy and imagecodecs (for lossless JPEG tiles).
Usage: tools/make_dng_fixtures.py OUTDIR"""
import struct, sys, zlib, os
import numpy as np
import imagecodecs

W, H = 64, 48

def srgb_enc(v):
    v = np.clip(v, 0, 1)
    return np.where(v <= 0.0031308, 12.92 * v, 1.055 * np.power(v, 1 / 2.4) - 0.055)

# Linear-light scene: every channel is linear in x and y, so bilinear demosaicing is exact inside.
yy, xx = np.mgrid[0:H, 0:W].astype(np.float64)
scene = np.stack([0.1 + 0.8 * xx / (W - 1), 0.1 + 0.8 * yy / (H - 1), 0.4 + 0.3 * (xx + yy) / (W + H - 2)], -1)
SRGB_TO_XYZ_D50 = np.array([[0.4360747, 0.3850649, 0.1430804], [0.2225045, 0.7168786, 0.0606169], [0.0139322, 0.0971045, 0.7141733]])
CM = 0.7 * np.array([[0.9, 0.2, -0.05], [0.1, 0.95, 0.05], [0.05, 0.1, 0.8]])  # XYZ(D50) -> camera
D50 = np.array([0.9642, 1.0, 0.8249])
NEUTRAL = CM @ D50
cam = (scene.reshape(-1, 3) @ SRGB_TO_XYZ_D50.T @ CM.T).reshape(H, W, 3)
assert cam.min() > 0 and cam.max() < 1, (cam.min(), cam.max())
FM = np.linalg.inv(CM) @ np.diag(NEUTRAL)

def expected():
    return (srgb_enc(scene) * 255 + 0.5).astype(np.uint8)

def mosaic(img, pattern):  # pattern: 2x2 of 0/1/2
    out = np.zeros((img.shape[0], img.shape[1]))
    for y in range(img.shape[0]):
        for x in range(img.shape[1]):
            out[y, x] = img[y, x, pattern[y % 2][x % 2]]
    return out

class Tiff:
    def __init__(self, le=True):
        self.le = le
        self.e = '<' if le else '>'
        self.buf = bytearray(b'II*\0' if le else b'MM\0*') + bytearray(4)
    def align(self):
        while len(self.buf) % 2: self.buf.append(0)
    def blob(self, data):
        self.align(); off = len(self.buf); self.buf += data; return off
    def ifd(self, entries):
        """entries: list of (tag, type, values). Returns the offset of the directory."""
        fmt = {1: 'B', 2: 'B', 3: 'H', 4: 'I', 5: 'I', 6: 'b', 7: 'B', 8: 'h', 9: 'i', 10: 'i', 11: 'f', 12: 'd'}
        rows = []
        for tag, typ, vals in sorted(entries, key=lambda e: e[0]):
            if typ == 2 and isinstance(vals, str): vals = list(vals.encode() + b'\0')
            if typ in (5, 10):
                flat = []
                for v in vals:
                    n, d = (v if isinstance(v, tuple) else (int(round(v * 10000)), 10000))
                    flat += [n, d]
                data = struct.pack(self.e + fmt[typ] * len(flat), *flat); count = len(vals)
            else:
                data = struct.pack(self.e + fmt[typ] * len(vals), *vals); count = len(vals)
            rows.append((tag, typ, count, data))
        stored = []
        for tag, typ, count, data in rows:
            stored.append(data.ljust(4, b'\0') if len(data) <= 4 else struct.pack(self.e + 'I', self.blob(data)))
        self.align(); off = len(self.buf)
        self.buf += struct.pack(self.e + 'H', len(rows))
        for (tag, typ, count, data), s in zip(rows, stored):
            self.buf += struct.pack(self.e + 'HHI', tag, typ, count) + s
        self.buf += struct.pack(self.e + 'I', 0)
        return off
    def finish(self, ifd0):
        self.buf[4:8] = struct.pack(self.e + 'I', ifd0)
        return bytes(self.buf)

def pack_bits(rows, bps, e):
    out = bytearray()
    for row in rows:
        acc = 0; n = 0
        for v in row:
            acc = (acc << bps) | int(v); n += bps
            while n >= 8:
                n -= 8; out.append((acc >> n) & 255); acc &= (1 << n) - 1
        if n: out.append((acc << (8 - n)) & 255)
    return bytes(out)

def color_tags(two=False, forward=False):
    t = [
        (50706, 1, [1, 4, 0, 0]), (50708, 2, 'Synthetic'),
        (50721, 10, [float(v) for v in CM.flatten()]), (50778, 3, [21]),
        (50728, 5, [float(v) for v in NEUTRAL]),
    ]
    if two:
        t += [(50722, 10, [float(v) for v in CM.flatten()]), (50779, 3, [17])]
    if forward:
        t += [(50964, 10, [float(v) for v in FM.flatten()]), (50965, 10, [float(v) for v in FM.flatten()])]
    return t

def make(name, out, *, le=True, bps=16, comp='none', pattern=((0, 1), (1, 2)), tiles=None, orientation=1,
         pad=None, crop=None, two=False, forward=False, black=0):
    t = Tiff(le)
    mos = mosaic(cam, pattern)
    white = (1 << bps) - 1
    full = np.round(black + mos * (white - black)).astype(np.int64)
    ax, ay = 0, 0
    if pad:  # extra border pixels around the active area, filled with junk
        top, left, bottom, right = pad
        junk = np.full((H + top + bottom, W + left + right), white // 3, dtype=np.int64)
        junk[top:top + H, left:left + W] = full
        full = junk; ax, ay = left, top
    h, w = full.shape
    e = '<' if le else '>'
    def encode(block):
        if comp == 'none':
            if bps == 16: return block.astype(e + 'u2').tobytes()
            return pack_bits(block, bps, e)
        if comp == 'deflate':
            b = block.astype(np.int64).copy()
            d = b.copy(); d[:, 1:] = (b[:, 1:] - b[:, :-1]) & 0xFFFF
            return zlib.compress(d.astype(e + 'u2').tobytes())
        if comp == 'ljpeg':
            return imagecodecs.ljpeg_encode(block.astype(np.uint16), bitspersample=bps)
    entries = [(254, 4, [0]), (256, 4, [w]), (257, 4, [h]), (258, 3, [bps]), (262, 3, [32803]),
               (277, 3, [1]), (284, 3, [1]), (33421, 3, [2, 2]), (33422, 1, [p for row in pattern for p in row]),
               (50717, 4, [white]), (50714, 3, [black])]
    entries.append((259, 3, [{'none': 1, 'deflate': 8, 'ljpeg': 7}[comp]]))
    if comp == 'deflate': entries.append((317, 3, [2]))
    if tiles:
        th, tw = tiles
        offs, cnts = [], []
        for ty in range(0, h, th):
            for tx in range(0, w, tw):
                blk = np.zeros((th, tw), dtype=np.int64)
                part = full[ty:ty + th, tx:tx + tw]
                blk[:part.shape[0], :part.shape[1]] = part
                d = encode(blk); offs.append(t.blob(d)); cnts.append(len(d))
        entries += [(322, 4, [tw]), (323, 4, [th]), (324, 4, offs), (325, 4, cnts)]
    else:
        rps = 16
        offs, cnts = [], []
        for ty in range(0, h, rps):
            d = encode(full[ty:ty + rps]); offs.append(t.blob(d)); cnts.append(len(d))
        entries += [(278, 4, [rps]), (273, 4, offs), (279, 4, cnts)]
    if pad:
        entries.append((50829, 4, [ay, ax, ay + H, ax + W]))
    if crop:
        entries += [(50719, 4, [crop[0], crop[1]]), (50720, 4, [crop[2], crop[3]])]
    raw_off = t.ifd(entries)
    # IFD 0: a tiny RGB preview plus the colour tags; the raw image lives in a SubIFD.
    prev = t.blob(bytes(3 * 4 * 4))
    ifd0 = [(254, 4, [1]), (256, 4, [4]), (257, 4, [4]), (258, 3, [8, 8, 8]), (259, 3, [1]), (262, 3, [2]),
            (273, 4, [prev]), (277, 3, [3]), (278, 4, [4]), (279, 4, [48]), (274, 3, [orientation]),
            (330, 4, [raw_off]), (50730, 10, [0.0])] + color_tags(two, forward)
    data = t.finish(t.ifd(ifd0))
    open(os.path.join(out, name), 'wb').write(data)

if __name__ == '__main__':
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    exp = expected()
    open(os.path.join(out, 'scene.ppm'), 'wb').write(b'P6\n%d %d\n255\n' % (W, H) + exp.tobytes())
    make('rggb_16bit_strips.dng', out)
    make('gbrg_12bit_packed_be.dng', out, le=False, bps=12, pattern=((1, 2), (0, 1)), black=64)
    make('ljpeg_tiles_14bit.dng', out, bps=14, comp='ljpeg', tiles=(16, 32), black=256)
    make('deflate_two_illuminants.dng', out, comp='deflate', two=True, forward=True)
    make('orientation6.dng', out, orientation=6)
    make('active_area_crop.dng', out, pad=(4, 6, 4, 2), crop=(2, 2, 60, 44))
    print('ok')
