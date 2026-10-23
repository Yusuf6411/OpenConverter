#!/usr/bin/env python3
"""Generate crates/oc-av1/src/tables.rs from the text of the AV1 specification, so no number is
typed by hand.

    python3 tools/av1_spec_text.py docs/specs/av1-spec.pdf /tmp/av1-spec.txt
    python3 tools/gen_av1_tables.py /tmp/av1-spec.txt crates/oc-av1/src/tables.rs
"""
import re
import sys

src, out = sys.argv[1], sys.argv[2]
text = open(src, encoding="utf-8").read()
text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
text = re.sub(r"//[^\n]*", " ", text)

C = {}
for i, n in enumerate(
    ["4X4", "4X8", "8X4", "8X8", "8X16", "16X8", "16X16", "16X32", "32X16", "32X32", "32X64",
     "64X32", "64X64", "64X128", "128X64", "128X128", "4X16", "16X4", "8X32", "32X8", "16X64", "64X16"]
):
    C["BLOCK_" + n] = i
C["BLOCK_INVALID"] = 22
for i, n in enumerate(
    ["4X4", "8X8", "16X16", "32X32", "64X64", "4X8", "8X4", "8X16", "16X8", "16X32", "32X16",
     "32X64", "64X32", "4X16", "16X4", "8X32", "32X8", "16X64", "64X16"]
):
    C["TX_" + n] = i
for i, n in enumerate(
    ["DCT_DCT", "ADST_DCT", "DCT_ADST", "ADST_ADST", "FLIPADST_DCT", "DCT_FLIPADST",
     "FLIPADST_FLIPADST", "ADST_FLIPADST", "FLIPADST_ADST", "IDTX", "V_DCT", "H_DCT", "V_ADST",
     "H_ADST", "V_FLIPADST", "H_FLIPADST"]
):
    C[n] = i
for i, n in enumerate(
    ["DC_PRED", "V_PRED", "H_PRED", "D45_PRED", "D135_PRED", "D113_PRED", "D157_PRED",
     "D203_PRED", "D67_PRED", "SMOOTH_PRED", "SMOOTH_V_PRED", "SMOOTH_H_PRED", "PAETH_PRED",
     "UV_CFL_PRED"]
):
    C[n] = i
for i, n in enumerate(["RESTORE_NONE", "RESTORE_WIENER", "RESTORE_SGRPROJ", "RESTORE_SWITCHABLE"]):
    C[n] = i
for i, n in enumerate(
    ["WEDGE_HORIZONTAL", "WEDGE_VERTICAL", "WEDGE_OBLIQUE27", "WEDGE_OBLIQUE63",
     "WEDGE_OBLIQUE117", "WEDGE_OBLIQUE153"]
):
    C[n] = i
for i, n in enumerate(["INTRA_FRAME", "LAST_FRAME", "LAST2_FRAME", "LAST3_FRAME", "GOLDEN_FRAME", "BWDREF_FRAME", "ALTREF2_FRAME", "ALTREF_FRAME"]):
    C[n] = i
C.update(
    MAX_LOOP_FILTER=63, SIG_COEF_CONTEXTS_2D=26, SIG_COEF_CONTEXTS=42, SIG_COEF_CONTEXTS_EOB=4,
    SEG_LVL_MAX=8, TX_SIZES_ALL=19, TX_SIZES=5, BLOCK_SIZES=22, INTRA_MODES=13,
    UV_INTRA_MODES_CFL_ALLOWED=14, UV_INTRA_MODES_CFL_NOT_ALLOWED=13, INTRA_MODE_CONTEXTS=5,
    BLOCK_SIZE_GROUPS=4, DIRECTIONAL_MODES=8, MAX_ANGLE_DELTA=3, PARTITION_CONTEXTS=4,
    TX_SIZE_CONTEXTS=3, MAX_TX_DEPTH=2, TXFM_PARTITION_CONTEXTS=21, SEGMENT_ID_CONTEXTS=3,
    MAX_SEGMENTS=8, SEGMENT_ID_PREDICTED_CONTEXTS=3, MV_OFFSET_BITS=10, NEW_MV_CONTEXTS=6,
    ZERO_MV_CONTEXTS=2, REF_MV_CONTEXTS=6, DRL_MODE_CONTEXTS=3, IS_INTER_CONTEXTS=4,
    COMP_INTER_CONTEXTS=5, SKIP_MODE_CONTEXTS=3, SKIP_CONTEXTS=3, REF_CONTEXTS=3, FWD_REFS=4,
    BWD_REFS=3, SINGLE_REFS=7, COMPOUND_MODE_CONTEXTS=8, COMPOUND_MODES=8,
    INTERP_FILTER_CONTEXTS=16, INTERP_FILTERS=3, MOTION_MODES=3, MV_JOINTS=4, MV_CLASSES=11,
    CLASS0_SIZE=2, PALETTE_BLOCK_SIZE_CONTEXTS=7, PALETTE_SIZES=7, PALETTE_COLOR_CONTEXTS=5,
    PALETTE_Y_MODE_CONTEXTS=3, PALETTE_UV_MODE_CONTEXTS=2, PALETTE_MAX_COLOR_CONTEXT_HASH=8,
    PALETTE_NUM_NEIGHBORS=3, DELTA_Q_SMALL=3, DELTA_LF_SMALL=3, COMPOUND_IDX_CONTEXTS=6,
    COMP_GROUP_IDX_CONTEXTS=6, COMPOUND_TYPES=2, INTERINTRA_MODES=4, COMP_REF_TYPE_CONTEXTS=5,
    UNIDIR_COMP_REFS=4, CFL_JOINT_SIGNS=8, CFL_ALPHA_CONTEXTS=6, CFL_ALPHABET_SIZE=16,
    COEFF_CDF_Q_CTXS=4, PLANE_TYPES=2, TXB_SKIP_CONTEXTS=13, EOB_COEF_CONTEXTS=9,
    DC_SIGN_CONTEXTS=3, LEVEL_CONTEXTS=21, BR_CDF_SIZE=4, TX_SET_TYPES_INTRA=3,
    TX_SET_TYPES_INTER=4, TX_TYPES=16, INTRA_FILTER_MODES=5, INTRA_EDGE_KERNELS=3,
    INTRA_EDGE_TAPS=5, SIG_REF_DIFF_OFFSET_NUM=5, COMP_NEWMV_CTXS=5, MASK_MASTER_SIZE=64,
    MAX_SB_SIZE=128, DIV_LUT_NUM=257, WARPEDPIXEL_PREC_SHIFTS=64, SUPERRES_FILTER_SHIFTS=64,
    SUPERRES_FILTER_TAPS=8, SGRPROJ_PARAMS_BITS=4, QM_TOTAL_SIZE=3344, REFS_PER_FRAME=7,
    WEDGE_TYPES=16, MAX_FRAME_DISTANCE=31,
)

# element type per table: by suffix, else by value range (see `ctype`)
FORCE = {}
SKIP = {"Block_Width", "Block_Height"}

pat = re.compile(r"^([A-Z][A-Za-z0-9]*_[A-Za-z0-9_]*)\s*((?:\[[^\]]*\]\s*)+)=\s*\{", re.M)


def parse(s, i):
    assert s[i] == "{"
    i += 1
    items, tok = [], ""
    while True:
        c = s[i]
        if c == "{":
            v, i = parse(s, i)
            items.append(v)
            continue
        if c in ",}":
            tok = tok.strip()
            # the PDF drops a comma now and then; a space between two plain words splits them
            items.extend(t for t in re.split(r"(?<=[A-Za-z0-9_])\s+(?=[A-Za-z0-9_])", tok) if t)
            tok = ""
            i += 1
            if c == "}":
                return items, i
            continue
        tok += c
        i += 1


def num(t):
    t = re.sub(r"\s+", " ", t.strip())
    e = re.sub(r"[A-Za-z_][A-Za-z0-9_]*", lambda m: str(C[m.group(0)]) if m.group(0) in C else m.group(0), t)
    if not re.fullmatch(r"[-+*/()<> 0-9]+", e):
        raise ValueError("unknown token %r" % t)
    return int(eval(e.replace("/", "//")))


def conv(v):
    return [conv(x) for x in v] if isinstance(v, list) else num(v)


def shape(v):
    if not isinstance(v, list):
        return []
    subs = [shape(x) for x in v]
    deep = max(subs, key=len) if subs else []
    dims = [len(v)]
    if deep:
        dims += [max(s[k] if k < len(s) else 0 for s in subs) for k in range(len(deep))]
    return dims


def pad(v, dims):
    if not dims:
        return v if not isinstance(v, list) else 0
    items = list(v) if isinstance(v, list) else []
    res = [pad(x, dims[1:]) for x in items]
    while len(res) < dims[0]:
        res.append(pad(0, dims[1:]))
    return res


def flat(v):
    if isinstance(v, list):
        for x in v:
            yield from flat(x)
    else:
        yield v


def emit(v, dims):
    if len(dims) == 1:
        return "[" + ", ".join(str(x) for x in v) + "]"
    return "[" + ",\n".join(emit(x, dims[1:]) for x in v) + "]"


def ctype(name, vals):
    lo, hi = min(vals), max(vals)
    if name.endswith("_Cdf") or "Scan" in name:
        return "u16"
    if lo >= 0:
        return "u8" if hi < 256 else "u16" if hi < 65536 else "u32"
    return "i8" if lo >= -128 and hi < 128 else "i16" if lo >= -32768 and hi < 32768 else "i32"


def rtype(t, dims):
    for d in reversed(dims):
        t = "[%s; %d]" % (t, d)
    return t


res = [
    "// Generated by tools/gen_av1_tables.py from the AV1 bitstream specification. Do not edit.",
    "#![allow(clippy::unreadable_literal, clippy::type_complexity, dead_code)]",
    "",
]
count = 0
for m in pat.finditer(text):
    name = m.group(1)
    if name in SKIP:
        continue
    decl = [num(d) for d in re.findall(r"\[([^\]]*)\]", m.group(2))]
    v, _ = parse(text, m.end() - 1)
    v = conv(v)
    dims = shape(v)
    if dims != decl:
        print("note: %s declared %s, data %s" % (name, decl, dims), file=sys.stderr)
        dims = decl if len(decl) == len(dims) and all(a >= b for a, b in zip(decl, dims)) else dims
    v = pad(v, dims)
    vals = list(flat(v))
    res.append("pub static %s: %s = %s;\n" % (name.upper(), rtype(ctype(name, vals), dims), emit(v, dims)))
    count += 1
open(out, "w").write("\n".join(res))
print("wrote", count, "tables to", out)
