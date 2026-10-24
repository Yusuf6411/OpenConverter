#!/usr/bin/env python3
"""Make WavPack test streams with ffmpeg's encoder and record ffmpeg's decoded samples.

Usage: make_wavpack_fixtures.py OUT_DIR

Each case writes NAME.wv and a line "NAME channels bits frames fnv" in MANIFEST. The hash is
FNV-1a 64 over the samples that ffmpeg's decoder produces, as signed 32-bit little-endian
values, left-justified (a 16-bit sample is shifted up by 16).
"""
import os, subprocess, sys
import numpy as np

def fnv(b):
    h = 0xCBF29CE484222325
    for x in b:
        h = ((h ^ x) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h

def signal(frames, channels, rate, seed):
    rng = np.random.default_rng(seed)
    t = np.arange(frames) / rate
    out = []
    for c in range(channels):
        s = 0.5 * np.sin(2 * np.pi * (220 + 110 * c) * t) + 0.25 * np.sin(2 * np.pi * (1330 + 7 * c) * t * (1 + 0.1 * np.sin(t * 3)))
        s += 0.02 * rng.standard_normal(frames)
        s[frames // 3: frames // 3 + frames // 8] = 0.0            # digital silence: zero runs
        s[frames // 2: frames // 2 + 400] *= 0.001                  # very quiet part
        out.append(np.clip(s, -1, 1))
    return np.stack(out, axis=1)

def write_wav(path, x, rate, bits):
    scale = {8: 127, 16: 32767, 24: 8388607, 32: 2147483647}[bits]
    ints = np.round(x * scale).astype(np.int64)
    if bits == 8:
        raw = (ints + 128).astype(np.uint8).tobytes()
    elif bits == 16:
        raw = ints.astype('<i2').tobytes()
    elif bits == 24:
        b = ints.astype('<i4').tobytes()
        raw = b''.join(b[i:i + 3] for i in range(0, len(b), 4))
    else:
        raw = ints.astype('<i4').tobytes()
    ch = x.shape[1]
    hdr = b'RIFF' + (36 + len(raw)).to_bytes(4, 'little') + b'WAVEfmt ' + (16).to_bytes(4, 'little')
    hdr += (1).to_bytes(2, 'little') + ch.to_bytes(2, 'little') + rate.to_bytes(4, 'little')
    hdr += (rate * ch * bits // 8).to_bytes(4, 'little') + (ch * bits // 8).to_bytes(2, 'little') + bits.to_bytes(2, 'little')
    hdr += b'data' + len(raw).to_bytes(4, 'little')
    open(path, 'wb').write(hdr + raw)

CASES = [
    # name, channels, bits, rate, frames, extra encoder args
    ('st16', 2, 16, 44100, 20000, []),
    ('st16_fast', 2, 16, 44100, 20000, ['-compression_level', '0']),
    ('st16_high', 2, 16, 44100, 20000, ['-compression_level', '3']),
    ('st16_nojoint', 2, 16, 44100, 20000, ['-joint_stereo', '0']),
    ('mono16', 1, 16, 48000, 20000, []),
    ('mono16_opt', 1, 16, 22050, 20000, ['-optimize_mono', '1']),
    ('st8', 2, 8, 8000, 9000, []),
    ('st24', 2, 24, 96000, 20000, []),
    ('st32', 2, 32, 44100, 12000, []),
    ('ch3', 3, 16, 44100, 8000, []),
    ('ch6', 6, 16, 48000, 8000, []),
    ('ch8', 8, 16, 48000, 6000, []),
    ('odd_rate', 2, 16, 37800, 7000, []),
    ('short', 2, 16, 44100, 17, []),
]

def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    lines = []
    for name, ch, bits, rate, frames, extra in CASES:
        wav = os.path.join(out, name + '.wav')
        wv = os.path.join(out, name + '.wv')
        write_wav(wav, signal(frames, ch, rate, sum(name.encode()) * 7919), rate, bits)
        codec_fmt = {8: 'u8p', 16: 's16p', 24: 's32p', 32: 's32p'}[bits]
        cmd = ['ffmpeg', '-loglevel', 'error', '-y', '-i', wav, '-c:a', 'wavpack', '-sample_fmt', codec_fmt]
        if bits == 24:
            cmd += ['-bits_per_raw_sample', '24']
        subprocess.run(cmd + extra + [wv], check=True)
        raw = subprocess.run(['ffmpeg', '-loglevel', 'error', '-i', wv, '-f', 's32le', '-c:a', 'pcm_s32le', '-'],
                             check=True, capture_output=True).stdout
        os.remove(wav)
        lines.append(f'{name} {ch} {bits} {len(raw) // (4 * ch)} {fnv(raw):016x}')
        print(lines[-1])
    open(os.path.join(out, 'MANIFEST'), 'w').write('\n'.join(lines) + '\n')

main()
