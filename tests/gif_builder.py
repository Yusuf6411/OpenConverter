"""Test-only GIF writer (OpenConverter 1.0 does not write GIF)."""

import struct


def lzw_encode(indices: bytes, min_code_size: int, *, clear_when_full=True, early_clear_every=None) -> bytes:
    clear = 1 << min_code_size
    eoi = clear + 1
    codes = []

    def reset():
        return {bytes([i]): i for i in range(clear)}, eoi + 1, min_code_size + 1

    table, next_code, size = reset()
    codes.append((clear, size))
    w = b""
    emitted = 0
    for b in indices:
        wc = w + bytes([b])
        if wc in table:
            w = wc
            continue
        codes.append((table[w], size))
        emitted += 1
        if next_code < 4096:
            table[wc] = next_code
            next_code += 1
            if next_code > (1 << size) and size < 12:
                size += 1
        elif clear_when_full:
            codes.append((clear, size))
            table, next_code, size = reset()
        if early_clear_every and emitted % early_clear_every == 0:
            codes.append((clear, size))
            table, next_code, size = reset()
        w = bytes([b])
    if w:
        codes.append((table[w], size))
    codes.append((eoi, size))
    acc = 0
    nbits = 0
    out = bytearray()
    for code, sz in codes:
        acc |= code << nbits
        nbits += sz
        while nbits >= 8:
            out.append(acc & 255)
            acc >>= 8
            nbits -= 8
    if nbits:
        out.append(acc & 255)
    return bytes(out)


def sub_blocks(data: bytes) -> bytes:
    out = bytearray()
    for i in range(0, len(data), 255):
        chunk = data[i:i + 255]
        out.append(len(chunk))
        out += chunk
    out.append(0)
    return bytes(out)


def palette_bytes(palette, size_bits):
    entries = list(palette) + [(0, 0, 0)] * ((2 << size_bits) - len(palette))
    return b"".join(bytes(e) for e in entries)


def build_gif(width, height, frames, global_palette=None, *, version=b"GIF89a", lzw_kwargs=None):
    """frames: list of dicts with keys indices, w, h, left, top, palette, transparent, interlace."""
    out = bytearray(version)
    gbits = 7
    flags = 0
    if global_palette is not None:
        gbits = max(0, (len(global_palette) - 1).bit_length() - 1)
        flags = 0x80 | (7 << 4) | gbits
    out += struct.pack("<HHBBB", width, height, flags, 0, 0)
    if global_palette is not None:
        out += palette_bytes(global_palette, gbits)
    out += b"\x21\xfe" + sub_blocks(b"made by the OpenConverter test suite")
    for fr in frames:
        w, h = fr.get("w", width), fr.get("h", height)
        if fr.get("transparent") is not None:
            out += b"\x21\xf9\x04" + bytes([0x01]) + struct.pack("<H", 10) + bytes([fr["transparent"]]) + b"\x00"
        fflags = 0
        local = fr.get("palette")
        lbits = 0
        if local is not None:
            lbits = max(0, (len(local) - 1).bit_length() - 1)
            fflags = 0x80 | lbits
        indices = fr["indices"]
        if fr.get("interlace"):
            fflags |= 0x40
            order = list(range(0, h, 8)) + list(range(4, h, 8)) + list(range(2, h, 4)) + list(range(1, h, 2))
            indices = b"".join(indices[y * w:(y + 1) * w] for y in order)
        out += b"\x2c" + struct.pack("<HHHHB", fr.get("left", 0), fr.get("top", 0), w, h, fflags)
        if local is not None:
            out += palette_bytes(local, lbits)
        mcs = max(2, fr.get("mcs", 8))
        out.append(mcs)
        out += sub_blocks(lzw_encode(indices, mcs, **(lzw_kwargs or {})))
    out += b"\x3b"
    return bytes(out)
