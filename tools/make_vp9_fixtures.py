#!/usr/bin/env python3
"""Make the VP9 test fixtures: IVF streams from libvpx-vp9 plus ffmpeg's decoded frames.

Usage: make_vp9_fixtures.py OUT_DIR [CASE...]   (needs ffmpeg with libvpx-vp9; the resize case
drives libvpx.so through ctypes to change the coded size mid-stream, which gives scaled references)
Each case writes NAME.ivf, NAME.yuv (planar, 8-bit or 16-bit little endian; for debugging) and
NAME.fnv (FNV-1a 64 of every decoded frame; this is what the tests compare against).
"""
import subprocess, sys, os

SRC = "testsrc2=size={s}:rate=10,format=yuv420p"
CASES = {
    # name: (size, frames, pix_fmt, extra encoder args)
    "kf":       ("176x144", 3, "yuv420p", ["-g", "1", "-crf", "30"]),
    "kf_odd":   ("66x50", 3, "yuv420p", ["-g", "1", "-crf", "30"]),
    "inter":    ("176x144", 12, "yuv420p", ["-g", "12", "-crf", "30", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "inter_odd": ("98x74", 12, "yuv420p", ["-g", "12", "-crf", "36", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "altref":   ("176x144", 20, "yuv420p", ["-g", "20", "-crf", "30", "-auto-alt-ref", "1", "-lag-in-frames", "10"]),
    "tiles":    ("512x144", 6, "yuv420p", ["-g", "6", "-crf", "30", "-tile-columns", "2", "-tile-rows", "1", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "lossless": ("64x48", 3, "yuv420p", ["-g", "3", "-lossless", "1", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "p1_444":   ("96x64", 4, "yuv444p", ["-g", "4", "-crf", "30", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "p1_422":   ("96x64", 4, "yuv422p", ["-g", "4", "-crf", "30", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "p2_10":    ("96x64", 4, "yuv420p10le", ["-g", "4", "-crf", "30", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "seg":      ("176x144", 8, "yuv420p", ["-g", "8", "-crf", "30", "-aq-mode", "3", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "errres":   ("176x144", 6, "yuv420p", ["-g", "6", "-crf", "30", "-error-resilient", "1", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
    "parallel": ("176x144", 6, "yuv420p", ["-g", "6", "-crf", "30", "-frame-parallel", "1", "-auto-alt-ref", "0", "-lag-in-frames", "0"]),
}

def resize_case(out_ivf):
    import ctypes, struct, subprocess
    lib = ctypes.CDLL("libvpx.so.9")
    lib.vpx_codec_vp9_cx.restype = ctypes.c_void_p
    iface = lib.vpx_codec_vp9_cx()
    cfg = ctypes.create_string_buffer(1024)
    lib.vpx_codec_enc_config_default.argtypes=[ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint]
    lib.vpx_codec_enc_config_default(iface, cfg, 0)
    def seti(off, v): struct.pack_into("<I", cfg, off, v)
    W0,H0 = 176,144
    seti(12,W0); seti(16,H0); seti(28,1); seti(32,10); seti(44,0); seti(112,300)
    ctx = ctypes.create_string_buffer(2048)
    lib.vpx_codec_enc_init_ver.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_void_p,ctypes.c_long,ctypes.c_int]
    ok=None
    for ver in range(1,80):
        r = lib.vpx_codec_enc_init_ver(ctx, iface, cfg, 0, ver)
        if r == 0: ok=ver;break
    print("abi", ok, file=sys.stderr)
    def frames(size, n, seed):
        w,h = size
        raw = subprocess.run(["ffmpeg","-v","error","-f","lavfi","-i","testsrc2=size=%dx%d:rate=10"%(w,h),"-vf","format=yuv420p","-frames:v",str(n+seed),"-f","rawvideo","-"],capture_output=True).stdout
        fb = w*h*3//2
        return [raw[i*fb:(i+1)*fb] for i in range(seed,n+seed)]
    plan = [((176,144),6),((132,108),6),((176,144),4),((100,80),4)]
    lib.vpx_img_alloc.restype = ctypes.c_void_p
    lib.vpx_img_alloc.argtypes=[ctypes.c_void_p,ctypes.c_int,ctypes.c_uint,ctypes.c_uint,ctypes.c_uint]
    lib.vpx_codec_encode.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_int64,ctypes.c_ulong,ctypes.c_long,ctypes.c_ulong]
    lib.vpx_codec_get_cx_data.restype=ctypes.c_void_p
    lib.vpx_codec_get_cx_data.argtypes=[ctypes.c_void_p,ctypes.c_void_p]
    lib.vpx_codec_enc_config_set.argtypes=[ctypes.c_void_p,ctypes.c_void_p]
    pkts=[]
    pts=0
    def drain():
        it = ctypes.c_void_p(0)
        while True:
            p = lib.vpx_codec_get_cx_data(ctx, ctypes.byref(it))
            if not p: break
            kind = ctypes.c_int.from_address(p).value
            if kind==0:
                buf = ctypes.c_void_p.from_address(p+8).value
                sz = ctypes.c_size_t.from_address(p+16).value
                pkts.append((pts, ctypes.string_at(buf, sz)))
    for (size,n) in plan:
        w,h = size
        seti(12,w); seti(16,h)
        r = lib.vpx_codec_enc_config_set(ctx, cfg)
        if r: print("config_set failed", r, file=sys.stderr)
        img = lib.vpx_img_alloc(None, 0x102, w, h, 1)
        planes = [ctypes.c_void_p.from_address(img+48+8*i).value for i in range(3)]
        strides = [ctypes.c_int.from_address(img+80+4*i).value for i in range(3)]
        for f in frames(size, n, 3):
            cw,ch=(w+1)//2,(h+1)//2
            offs=[(0,w,h),(w*h,cw,ch),(w*h+cw*ch,cw,ch)]
            for pi,(o,pw,ph) in enumerate(offs):
                for y in range(ph):
                    ctypes.memmove(planes[pi]+y*strides[pi], f[o+y*pw:o+(y+1)*pw], pw)
            r = lib.vpx_codec_encode(ctx, img, pts, 1, 0, 1)  # good quality
            if r: print("encode failed", r, file=sys.stderr)
            drain(); pts+=1
    r = lib.vpx_codec_encode(ctx, None, pts, 1, 0, 1); drain()
    with open(out_ivf,"wb") as f:
        f.write(b"DKIF"+struct.pack("<HH4sHHIII",0,32,b"VP90",W0,H0,10,1,len(pkts))+b"\0\0\0\0")
        for t,d in pkts:
            f.write(struct.pack("<IQ",len(d),t)+d)
    print(len(pkts),"packets", file=sys.stderr)

    return plan


def run(cmd):
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def fnv(data):
    h = 0xcbf29ce484222325
    for b in data:
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h

def frame_bytes(w, h, pix):
    sx = 2 if pix in ("yuv420p", "yuv422p", "yuv420p10le") else 1
    sy = 2 if pix in ("yuv420p", "yuv420p10le") else 1
    bps = 2 if "10" in pix else 1
    cw, ch = (w + sx - 1) // sx, (h + sy - 1) // sy
    return (w * h + 2 * cw * ch) * bps

def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    only = sys.argv[2:] or list(CASES) + ["resize"]
    if "resize" in only:
        only.remove("resize")
        ivf = os.path.join(out, "resize.ivf")
        yuv = os.path.join(out, "resize.yuv")
        plan = resize_case(ivf)
        run(["ffmpeg", "-y", "-i", ivf, "-autoscale", "0", "-f", "rawvideo", "-pix_fmt", "yuv420p", yuv])
        data = open(yuv, "rb").read()
        with open(os.path.join(out, "resize.fnv"), "w") as f:
            at = 0
            for (w, h), n in plan:
                fb = frame_bytes(w, h, "yuv420p")
                for _ in range(n):
                    f.write("%016x\n" % fnv(data[at:at + fb]))
                    at += fb
    for name in only:
        size, n, pix, extra = CASES[name]
        ivf = os.path.join(out, name + ".ivf")
        yuv = os.path.join(out, name + ".yuv")
        profile = "1" if pix in ("yuv444p", "yuv422p") else ("2" if "10" in pix else "0")
        run(["ffmpeg", "-y", "-f", "lavfi", "-i", SRC.format(s=size), "-frames:v", str(n),
             "-pix_fmt", pix, "-c:v", "libvpx-vp9", "-profile:v", profile, "-b:v", "0",
             "-cpu-used", "4", "-threads", "1"] + extra + ["-f", "ivf", ivf])
        run(["ffmpeg", "-y", "-i", ivf, "-f", "rawvideo", "-pix_fmt", pix, yuv])
        w, h = map(int, size.split("x"))
        data = open(yuv, "rb").read()
        fb = frame_bytes(w, h, pix)
        with open(os.path.join(out, name + ".fnv"), "w") as f:
            for i in range(len(data) // fb):
                f.write("%016x\n" % fnv(data[i * fb:(i + 1) * fb]))

main()
