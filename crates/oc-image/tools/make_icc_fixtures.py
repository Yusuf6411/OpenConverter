"""ICC profile fixtures for tests/icc.rs.

Profiles are built here by hand (matrix/TRC with every curve type, gray, mft1, mft2 with
Lab and XYZ PCS, mAB for RGB and CMYK) and the expected sRGB pixels come from Little CMS
through Pillow's ImageCms.

Run: python3 make_icc_fixtures.py ../tests/data/icc
"""
import io
import math
import os
import struct
import sys

import numpy as np
from PIL import ImageCms, Image

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
rng = np.random.default_rng(5)
D50 = (0.9642, 1.0, 0.8249)

SRGB_D50 = np.array([
    [0.4360747, 0.3850649, 0.1430804],
    [0.2225045, 0.7168786, 0.0606169],
    [0.0139322, 0.0971045, 0.7141733],
])


def s15(v):
    return struct.pack(">i", int(round(v * 65536)))


def xyz_tag(x, y, z):
    return b"XYZ \0\0\0\0" + s15(x) + s15(y) + s15(z)


def pad4(b):
    return b + b"\0" * (-len(b) % 4)


def curv_gamma(g):
    return pad4(b"curv\0\0\0\0" + struct.pack(">IH", 1, int(round(g * 256))))


def curv_table(vals):
    v = np.clip(np.round(np.asarray(vals) * 65535), 0, 65535).astype(">u2")
    return pad4(b"curv\0\0\0\0" + struct.pack(">I", len(v)) + v.tobytes())


def curv_identity():
    return b"curv\0\0\0\0" + struct.pack(">I", 0)


def para(kind, params):
    return pad4(b"para\0\0\0\0" + struct.pack(">HH", kind, 0) + b"".join(s15(p) for p in params))


def srgb_eotf(x):
    x = np.asarray(x, float)
    return np.where(x <= 0.04045, x / 12.92, ((x + 0.055) / 1.055) ** 2.4)


def profile(space, pcs, tags, version=0x02400000, cls=b"mntr"):
    names = list(tags)
    count = len(names)
    off = 128 + 4 + 12 * count
    table = struct.pack(">I", count)
    body = b""
    for n in names:
        data = pad4(tags[n])
        table += n.encode() + struct.pack(">II", off + len(body), len(tags[n]))
        body += data
    size = off + len(body)
    head = struct.pack(">I4sI4s4s4s", size, b"\0\0\0\0", version, cls, space, pcs)
    head += b"\0" * 12 + b"acsp" + b"\0" * 4 + b"\0" * 4 + b"\0" * 8 + b"\0" * 8 + struct.pack(">I", 0)
    head += s15(D50[0]) + s15(D50[1]) + s15(D50[2]) + b"\0" * 4 + b"\0" * 44
    assert len(head) == 128, len(head)
    return head + table + body


def desc(text):
    t = text.encode() + b"\0"
    return pad4(b"desc\0\0\0\0" + struct.pack(">I", len(t)) + t + b"\0" * 78)


def common(extra):
    t = {"desc": desc("fixture"), "wtpt": xyz_tag(*D50)}
    t.update(extra)
    return t


def matrix_profile(m, curves, version=0x02400000):
    tags = common({
        "rXYZ": xyz_tag(*m[:, 0]), "gXYZ": xyz_tag(*m[:, 1]), "bXYZ": xyz_tag(*m[:, 2]),
        "rTRC": curves[0], "gTRC": curves[1], "bTRC": curves[2],
    })
    return profile(b"RGB ", b"XYZ ", tags, version)


# --- colour maths for the LUT fixtures -------------------------------------
def xyz_to_lab(xyz):
    t = np.asarray(xyz) / np.array(D50)
    f = np.where(t > (6 / 29) ** 3, np.cbrt(t), t / (3 * (6 / 29) ** 2) + 4 / 29)
    return np.stack([116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])], -1)


def device_rgb_to_xyz(rgb):
    """A made-up RGB device: slightly rotated primaries and a 2.0 gamma."""
    lin = np.clip(rgb, 0, 1) ** 2.0
    mix = np.array([[0.9, 0.1, 0.0], [0.05, 0.9, 0.05], [0.0, 0.1, 0.9]])
    return (lin @ mix.T) @ SRGB_D50.T


def device_cmyk_to_xyz(cmyk):
    c, m, y, k = [cmyk[..., i] for i in range(4)]
    r = (1 - c * 0.95) * (1 - k * 0.97)
    g = (1 - m * 0.9) * (1 - k * 0.97)
    b = (1 - y * 0.85) * (1 - k * 0.97)
    return np.stack([r, g, b], -1) @ SRGB_D50.T


def grid_inputs(n_in, g):
    axes = np.meshgrid(*[np.linspace(0, 1, g)] * n_in, indexing="ij")
    return np.stack(axes, -1).reshape(-1, n_in)


def lab_enc_v2(lab):
    L = lab[..., 0] / 100 * 65280
    a = (lab[..., 1] + 128) * 256
    b = (lab[..., 2] + 128) * 256
    return np.clip(np.round(np.stack([L, a, b], -1)), 0, 65535)


def lab_enc_8(lab):
    return np.clip(np.round(np.stack([lab[..., 0] / 100 * 255, lab[..., 1] + 128, lab[..., 2] + 128], -1)), 0, 255)


def lab_enc_v4(lab):
    return np.clip(np.round(np.stack([lab[..., 0] / 100 * 65535, (lab[..., 1] + 128) / 255 * 65535, (lab[..., 2] + 128) / 255 * 65535], -1)), 0, 65535)


def xyz_enc(xyz):
    return np.clip(np.round(xyz * 32768), 0, 65535)


IDENT_M = b"".join(s15(v) for v in (1, 0, 0, 0, 1, 0, 0, 0, 1))


def mft2(n_in, g, clut, in_tabs, out_tabs):
    ne_in, ne_out = len(in_tabs[0]), len(out_tabs[0])
    b = b"mft2\0\0\0\0" + bytes([n_in, 3, g, 0]) + IDENT_M + struct.pack(">HH", ne_in, ne_out)
    for t in in_tabs:
        b += np.asarray(t, ">u2").tobytes()
    b += np.asarray(clut, ">u2").tobytes()
    for t in out_tabs:
        b += np.asarray(t, ">u2").tobytes()
    return b


def mft1(n_in, g, clut, in_tabs, out_tabs):
    b = b"mft1\0\0\0\0" + bytes([n_in, 3, g, 0]) + IDENT_M
    for t in in_tabs:
        b += np.asarray(t, "u1").tobytes()
    b += np.asarray(clut, "u1").tobytes()
    for t in out_tabs:
        b += np.asarray(t, "u1").tobytes()
    return b


def curves_block(cs):
    return b"".join(cs)


def mab(n_in, g, clut16, a_curves, m_curves, matrix, b_curves):
    """matrix: 12 numbers or None. Elements laid out B, matrix, M, CLUT, A."""
    parts = {}
    pos = 32
    blobs = []

    def put(name, data):
        nonlocal pos
        data = pad4(data)
        parts[name] = pos
        blobs.append(data)
        pos += len(data)

    put("B", curves_block(b_curves))
    if matrix is not None:
        put("matrix", b"".join(s15(v) for v in matrix))
    if m_curves:
        put("M", curves_block(m_curves))
    grid = bytes([g] * n_in) + b"\0" * (16 - n_in) + bytes([2, 0, 0, 0])
    put("clut", grid + np.asarray(clut16, ">u2").tobytes())
    put("A", curves_block(a_curves))
    head = b"mAB \0\0\0\0" + bytes([n_in, 3, 0, 0])
    head += struct.pack(">IIIII", parts["B"], parts.get("matrix", 0), parts.get("M", 0), parts["clut"], parts["A"])
    return head + b"".join(blobs)


fixtures = {}  # name -> (profile bytes, channels)

# --- matrix / TRC -----------------------------------------------------------
P3_D50 = np.array([[0.5151, 0.2920, 0.1571], [0.2412, 0.6922, 0.0666], [-0.0011, 0.0419, 0.7841]])
ADOBE_D50 = np.array([[0.6097559, 0.2052401, 0.1492240], [0.3111242, 0.6256560, 0.0632197], [0.0194811, 0.0608902, 0.7448387]])
WIDE_D50 = np.array([[0.7977, 0.1352, 0.0313], [0.2880, 0.7119, 0.0001], [0.0, 0.0, 0.8249]])
tbl = srgb_eotf(np.linspace(0, 1, 1024))
fixtures["m_gamma22"] = (matrix_profile(ADOBE_D50, [curv_gamma(2.2)] * 3), 3)
fixtures["m_gamma_mixed"] = (matrix_profile(P3_D50, [curv_gamma(1.8), curv_gamma(2.2), curv_gamma(2.6)]), 3)
fixtures["m_table"] = (matrix_profile(P3_D50, [curv_table(tbl)] * 3), 3)
fixtures["m_short_table"] = (matrix_profile(WIDE_D50, [curv_table(np.linspace(0, 1, 17) ** 2.4)] * 3), 3)
fixtures["m_identity_curve"] = (matrix_profile(SRGB_D50, [curv_identity()] * 3), 3)
fixtures["m_para0"] = (matrix_profile(ADOBE_D50, [para(0, [2.2])] * 3, 0x04200000), 3)
fixtures["m_para1"] = (matrix_profile(P3_D50, [para(1, [2.4, 1 / 1.055, 0.055 / 1.055])] * 3, 0x04200000), 3)
fixtures["m_para2"] = (matrix_profile(P3_D50, [para(2, [2.4, 1 / 1.055, 0.055 / 1.055, 0.02])] * 3, 0x04200000), 3)
fixtures["m_para3"] = (matrix_profile(P3_D50, [para(3, [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045])] * 3, 0x04200000), 3)
fixtures["m_para4"] = (matrix_profile(WIDE_D50, [para(4, [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045, 0.01, 0.002])] * 3, 0x04200000), 3)
fixtures["m_linear"] = (matrix_profile(WIDE_D50, [para(0, [1.0])] * 3, 0x04200000), 3)
fixtures["g_gamma18"] = (profile(b"GRAY", b"XYZ ", common({"kTRC": curv_gamma(1.8)}), cls=b"mntr"), 1)
fixtures["g_table"] = (profile(b"GRAY", b"XYZ ", common({"kTRC": curv_table(tbl)})), 1)
fixtures["g_para"] = (profile(b"GRAY", b"XYZ ", common({"kTRC": para(3, [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045])}), 0x04200000), 1)

# --- LUT based --------------------------------------------------------------
g = 9
rgb_grid = grid_inputs(3, g)
xyz = device_rgb_to_xyz(rgb_grid)
lab = xyz_to_lab(xyz)
curve_in = [np.round(np.linspace(0, 1, 64) ** 0.9 * 65535) for _ in range(3)]
curve_out_id = [np.round(np.linspace(0, 65535, 2)) for _ in range(3)]
fixtures["lut16_rgb_lab"] = (profile(b"RGB ", b"Lab ", common({"A2B0": mft2(3, g, lab_enc_v2(lab), curve_in, curve_out_id)})), 3)
fixtures["lut16_rgb_xyz"] = (profile(b"RGB ", b"XYZ ", common({"A2B0": mft2(3, g, xyz_enc(xyz), curve_in, curve_out_id)})), 3)
fixtures["lut8_rgb_lab"] = (profile(b"RGB ", b"Lab ", common({"A2B0": mft1(3, g, lab_enc_8(lab), [np.arange(256)] * 3, [np.arange(256)] * 3)})), 3)
# a second table for relative colorimetric, to prove the intent picks the right one
xyz_b = device_rgb_to_xyz(np.clip(rgb_grid * 0.9 + 0.05, 0, 1))
fixtures["lut16_intents"] = (profile(b"RGB ", b"Lab ", common({
    "A2B0": mft2(3, g, lab_enc_v2(lab), curve_in, curve_out_id),
    "A2B1": mft2(3, g, lab_enc_v2(xyz_to_lab(xyz_b)), curve_in, curve_out_id),
})), 3)

gk = 7
cmyk_grid = grid_inputs(4, gk)
lab_k = xyz_to_lab(device_cmyk_to_xyz(cmyk_grid))
cin = [np.round(np.linspace(0, 1, 32) ** 1.1 * 65535) for _ in range(4)]
fixtures["lut16_cmyk_lab"] = (profile(b"CMYK", b"Lab ", common({"A2B0": mft2(4, gk, lab_enc_v2(lab_k), cin, curve_out_id)}), cls=b"prtr"), 4)
fixtures["lut8_cmyk_lab"] = (profile(b"CMYK", b"Lab ", common({"A2B0": mft1(4, gk, lab_enc_8(lab_k), [np.arange(256)] * 4, [np.arange(256)] * 3)}), cls=b"prtr"), 4)

# mAB, RGB, Lab PCS, v4: A curves, CLUT, M curves, matrix, B curves
fixtures["mab_rgb_lab"] = (profile(b"RGB ", b"Lab ", common({"A2B0": mab(3, g, lab_enc_v4(lab), [curv_identity()] * 3, [], None, [curv_identity()] * 3)}), 0x04200000), 3)
fixtures["mab_rgb_curves"] = (profile(b"RGB ", b"Lab ", common({"A2B0": mab(3, g, lab_enc_v4(lab), [curv_table(np.linspace(0, 1, 40) ** 0.8)] * 3, [], None, [para(0, [1.0])] * 3)}), 0x04200000), 3)
fixtures["mab_cmyk_lab"] = (profile(b"CMYK", b"Lab ", common({"A2B0": mab(4, gk, lab_enc_v4(lab_k), [curv_table(np.linspace(0, 1, 40) ** 1.1)] * 4, [], None, [curv_identity()] * 3)}), 0x04200000, b"prtr"), 4)

# lcms's own sRGB, which must come out unchanged
srgb_bytes = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes()
fixtures["lcms_srgb"] = (srgb_bytes, 3)

target = ImageCms.createProfile("sRGB")
for name, (data, ch) in fixtures.items():
    open(f"{out}/{name}.icc", "wb").write(data)
    n = 1500
    pix = rng.integers(0, 256, size=(n, ch), dtype=np.uint8)
    # corners and axes
    extra = [[v if i == j else 0 for i in range(ch)] for j in range(ch) for v in (0, 128, 255)]
    extra += [[255] * ch, [0] * ch, [128] * ch]
    pix = np.concatenate([np.array(extra, np.uint8), pix])
    src = ImageCms.ImageCmsProfile(io.BytesIO(data))
    mode = {1: "L", 3: "RGB", 4: "CMYK"}[ch]
    noopt, bpc = 0x0100, 0x2000  # cmsFLAGS_NOOPTIMIZE, cmsFLAGS_BLACKPOINTCOMPENSATION
    # i0: perceptual (Little CMS forces black point compensation towards its v4 sRGB),
    # i1: relative colorimetric, i1b: relative colorimetric with black point compensation.
    for tag, intent, flags in (("i0", 0, noopt), ("i1", 1, noopt), ("i1b", 1, noopt | bpc)):
        t = ImageCms.buildTransform(src, target, mode, "RGB", renderingIntent=intent, flags=flags)
        im = Image.frombytes(mode, (len(pix), 1), pix.tobytes())
        res = np.array(ImageCms.applyTransform(im, t)).reshape(-1, 3)
        open(f"{out}/{name}.{tag}.rgb", "wb").write(res.astype(np.uint8).tobytes())
    open(f"{out}/{name}.in", "wb").write(pix.tobytes())
    print(name, len(data), ch)

# A CMYK JPEG that carries a CMYK profile; Pillow writes it with Adobe's inverted samples and
# un-inverts on load, so what it reads back is ink amounts, as the profile expects.
cm = np.zeros((24, 32, 4), np.uint8)
for y in range(24):
    for x in range(32):
        cm[y, x] = [x * 8, y * 10, (x * y) % 256, (x + y) * 4 % 256]
im = Image.fromarray(cm, "CMYK")
im.save(f"{out}/cmyk_profile.jpg", quality=95, subsampling=0, icc_profile=fixtures["lut16_cmyk_lab"][0])
back = Image.open(f"{out}/cmyk_profile.jpg")
back.load()
src = ImageCms.ImageCmsProfile(io.BytesIO(fixtures["lut16_cmyk_lab"][0]))
t = ImageCms.buildTransform(src, target, "CMYK", "RGB", renderingIntent=1, flags=0x0100 | 0x2000)
rgb = np.array(ImageCms.applyTransform(back, t))
open(f"{out}/cmyk_profile.rgb", "wb").write(rgb.astype(np.uint8).tobytes())
