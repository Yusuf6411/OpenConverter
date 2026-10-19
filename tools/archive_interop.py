#!/usr/bin/env python3
"""Cross-check oc-archive against Python's zipfile/tarfile and system tools.
Usage: python3 tools/archive_interop.py   (needs a built archive_tool example)."""
import hashlib, io, os, shutil, subprocess, sys, tarfile, tempfile, zipfile, zlib, stat, random

TOOL = os.environ.get("ARCHIVE_TOOL", "target/debug/examples/archive_tool")
rnd = random.Random(7)
def blob(n): return bytes(rnd.getrandbits(8) for _ in range(n))
FILES = {
    "hello.txt": b"hello world\n" * 50,
    "dir/sub/data.bin": blob(5000),
    "dir/empty": b"",
    "unicode/héllo-ñ-日本語.txt": "unicode name ✓\n".encode() * 20,
    "big.txt": (b"The quick brown fox jumps over the lazy dog. " * 4000),
    "long/" + "d" * 60 + "/" + "e" * 60 + "/" + "f" * 60 + ".txt": b"long path\n",
}
fail = 0
def check(cond, msg):
    global fail
    print(("ok   " if cond else "FAIL ") + msg)
    if not cond: fail += 1

def run(*a, env=None, ok=True):
    e = dict(os.environ); e.update(env or {})
    r = subprocess.run([TOOL, *a], capture_output=True, text=True, env=e)
    if ok and r.returncode: print(r.stderr); 
    return r

def listing(path, env=None):
    r = run("list", path, env=env)
    out = {}
    for line in r.stdout.splitlines():
        k, mode, size, crc, name = line.split(" ", 4)
        out[name] = (k, int(size), int(crc))
    return out

def expected():
    d = {}
    for n, b in FILES.items():
        d[n] = ("f", len(b), zlib.crc32(b))
    return d

tmp = tempfile.mkdtemp()
# --- fixtures with Python writers, read by us
src = os.path.join(tmp, "src")
for n, b in FILES.items():
    p = os.path.join(src, n); os.makedirs(os.path.dirname(p), exist_ok=True); open(p, "wb").write(b)

def only_files(l): return {k: v for k, v in l.items() if v[0] == "f" and not k.startswith("adir/")}
for comp, name in [(zipfile.ZIP_STORED, "stored"), (zipfile.ZIP_DEFLATED, "deflate"), (zipfile.ZIP_BZIP2, "bzip2"), (zipfile.ZIP_LZMA, "lzma")]:
    p = os.path.join(tmp, f"py_{name}.zip")
    with zipfile.ZipFile(p, "w", comp) as z:
        for n, b in FILES.items(): z.writestr(n, b)
        z.writestr("adir/", b"")
    check(only_files(listing(p)) == expected(), f"read python zip {name}")
for mode, name in [("w", "tar"), ("w:gz", "tar.gz"), ("w:bz2", "tar.bz2"), ("w:xz", "tar.xz")]:
    for fmt in (tarfile.USTAR_FORMAT, tarfile.GNU_FORMAT, tarfile.PAX_FORMAT):
        p = os.path.join(tmp, f"py_{fmt}.{name}")
        with tarfile.open(p, mode, format=fmt) as t:
            t.add(src, arcname="", recursive=True) if False else None
            for n, b in FILES.items():
                ti = tarfile.TarInfo(n); ti.size = len(b); ti.mtime = 1700000000
                t.addfile(ti, io.BytesIO(b))
        check(only_files(listing(p)) == expected(), f"read python tar fmt={fmt} {name}")

# --- our writers, read by Python / system tools
for ext in ["zip", "tar", "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "cpio", "7z"]:
    out = os.path.join(tmp, "ours." + ext)
    r = run("pack", src, out)
    if r.returncode: check(False, f"pack {ext}: {r.stderr.strip()}"); continue
    check(only_files(listing(out)) == expected(), f"roundtrip pack/list {ext}")
    if ext == "zip":
        with zipfile.ZipFile(out) as z:
            check(z.testzip() is None and {n: z.read(n) for n in z.namelist() if not n.endswith("/")} == FILES, "python reads our zip")
        if shutil.which("unzip"):
            check(subprocess.run(["unzip", "-tq", out], capture_output=True).returncode == 0, "unzip -t accepts our zip")
    if ext.startswith("tar") and not ext.endswith("zst"):
        with tarfile.open(out) as t:
            got = {m.name: t.extractfile(m).read() for m in t.getmembers() if m.isfile()}
            check(got == FILES, f"python reads our {ext}")
        if ext == "tar" and shutil.which("tar"):
            check(subprocess.run(["tar", "tf", out], capture_output=True).returncode == 0, "system tar lists ours")

# --- ZIP specifics
p = os.path.join(tmp, "z64.zip")
with zipfile.ZipFile(p, "w", zipfile.ZIP_DEFLATED) as z:
    for n, b in FILES.items():
        with z.open(n, "w", force_zip64=True) as f: f.write(b)
check(only_files(listing(p)) == expected(), "read ZIP64-flagged python zip")
for method in ["stored", "deflate", "bzip2", "lzma", "zstd", "xz", "auto"]:
    out = os.path.join(tmp, f"m_{method}.zip")
    run("convert", os.path.join(tmp, "py_deflate.zip"), out, env={"OC_ZIP_METHOD": method})
    ok = only_files(listing(out)) == expected()
    if method in ("stored", "deflate", "bzip2", "lzma", "auto"):
        with zipfile.ZipFile(out) as z:
            ok = ok and z.testzip() is None and all(z.read(n) == b for n, b in FILES.items())
    check(ok, f"zip write method={method}")
# encryption
for enc in ["zipcrypto", "aes256", "aes128"]:
    out = os.path.join(tmp, f"enc_{enc}.zip")
    run("convert", os.path.join(tmp, "py_deflate.zip"), out, env={"OC_OUT_PASSWORD": "s3cret", "OC_ZIP_ENC": enc})
    check(only_files(listing(out, env={"OC_PASSWORD": "s3cret"})) == expected(), f"decrypt our {enc} zip")
    r = run("list", out, env={"OC_PASSWORD": "wrong"}, ok=False)
    check(r.returncode != 0 and "wrong_password" in r.stderr, f"{enc}: wrong password rejected")
    r = run("list", out, ok=False)
    check(r.returncode != 0 and "encrypted_need_password" in r.stderr, f"{enc}: missing password reported")
    if enc == "zipcrypto" and shutil.which("unzip"):
        r = subprocess.run(["unzip", "-tq", "-P", "s3cret", out], capture_output=True)
        check(r.returncode == 0, "unzip decrypts our ZipCrypto")
try:
    import pyzipper
    with pyzipper.AESZipFile(os.path.join(tmp, "enc_aes256.zip")) as z:
        z.setpassword(b"s3cret"); check(all(z.read(n) == b for n, b in FILES.items()), "pyzipper reads our AES-256")
    p = os.path.join(tmp, "pyaes.zip")
    with pyzipper.AESZipFile(p, "w", compression=zipfile.ZIP_DEFLATED, encryption=pyzipper.WZ_AES) as z:
        z.setpassword(b"pw"); [z.writestr(n, b) for n, b in FILES.items()]
    check(only_files(listing(p, env={"OC_PASSWORD": "pw"})) == expected(), "read pyzipper AES zip")
except ImportError:
    print("skip pyzipper checks")
if shutil.which("zip"):
    p = os.path.join(tmp, "zip_pw.zip")
    subprocess.run(["zip", "-q", "-r", "-P", "pw", p, "."], cwd=src, check=True)
    check(only_files(listing(p, env={"OC_PASSWORD": "pw"})) == expected(), "read Info-ZIP encrypted zip")
    p = os.path.join(tmp, "zip_plain.zip")
    subprocess.run(["zip", "-q", "-r", "-y", p, "."], cwd=src, check=True)
    check(only_files(listing(p)) == expected(), "read Info-ZIP zip")
# cross-conversion
for a, b in [("zip", "tar.gz"), ("tar.xz", "zip"), ("tar", "cpio"), ("cpio", "zip")]:
    pa = os.path.join(tmp, "ours." + a); pb = os.path.join(tmp, f"x_{a}.{b}")
    if not os.path.exists(pa): continue
    run("convert", pa, pb)
    check(only_files(listing(pb)) == expected(), f"convert {a} -> {b}")
# cpio via system cpio
if shutil.which("cpio"):
    p = os.path.join(tmp, "ours.cpio")
    r = subprocess.run(["cpio", "-it"], stdin=open(p, "rb"), capture_output=True)
    check(r.returncode == 0, "system cpio lists ours")
# ar
if shutil.which("ar"):
    arsrc = os.path.join(tmp, "arsrc"); os.makedirs(arsrc)
    for n in ["a.o", "a-very-long-member-name.o"]: open(os.path.join(arsrc, n), "wb").write(blob(333))
    p = os.path.join(tmp, "sys.ar")
    subprocess.run(["ar", "rc", p, "a.o", "a-very-long-member-name.o"], cwd=arsrc, check=True)
    l = listing(p)
    check(set(l) == {"a.o", "a-very-long-member-name.o"}, "read system ar")
    out = os.path.join(tmp, "ours.ar"); run("convert", p, out)
    r = subprocess.run(["ar", "t", out], capture_output=True, text=True)
    check(sorted(r.stdout.split()) == ["a-very-long-member-name.o", "a.o"], "system ar lists ours")

# --- 7z
try:
    import py7zr
except ImportError:
    py7zr = None
if py7zr:
    def mk7z(name, filters, password=None, header_enc=False):
        p = os.path.join(tmp, name)
        with py7zr.SevenZipFile(p, "w", filters=filters, password=password, header_encryption=header_enc) as z:
            for n, b in FILES.items(): z.writestr(b, n)
            z.writestr(b"", "adir/keep.txt")
        return p
    cases = {
        "lzma2": [{"id": py7zr.FILTER_LZMA2, "preset": 5}],
        "lzma": [{"id": py7zr.FILTER_LZMA, "preset": 5}],
        "bzip2": [{"id": py7zr.FILTER_BZIP2}],
        "deflate": [{"id": py7zr.FILTER_DEFLATE}],
        "copy": [{"id": py7zr.FILTER_COPY}],
        "bcj+lzma2": [{"id": py7zr.FILTER_X86}, {"id": py7zr.FILTER_LZMA2, "preset": 3}],
        "delta+lzma2": [{"id": py7zr.FILTER_DELTA, "dist": 4}, {"id": py7zr.FILTER_LZMA2, "preset": 3}],
        "arm+lzma": [{"id": py7zr.FILTER_ARM}, {"id": py7zr.FILTER_LZMA, "preset": 3}],
    }
    for name, f in cases.items():
        p = mk7z(f"py_{name}.7z", f)
        check(only_files(listing(p)) == expected(), f"read py7zr 7z {name}")
    p = mk7z("py_aes.7z", [{"id": py7zr.FILTER_LZMA2, "preset": 3}, {"id": py7zr.FILTER_CRYPTO_AES256_SHA256}], password="pw7")
    check(only_files(listing(p, env={"OC_PASSWORD": "pw7"})) == expected(), "read py7zr AES 7z")
    r = run("list", p, env={"OC_PASSWORD": "nope"}, ok=False)
    check(r.returncode != 0 and "wrong_password" in r.stderr, "7z wrong password rejected")
    r = run("list", p, ok=False)
    check(r.returncode != 0 and "encrypted_need_password" in r.stderr, "7z missing password reported")
    p = mk7z("py_hdrenc.7z", [{"id": py7zr.FILTER_LZMA2, "preset": 3}, {"id": py7zr.FILTER_CRYPTO_AES256_SHA256}], password="pw7", header_enc=True)
    check(only_files(listing(p, env={"OC_PASSWORD": "pw7"})) == expected(), "read py7zr 7z with encrypted header")
    r = run("list", p, ok=False)
    check(r.returncode != 0 and "encrypted_need_password" in r.stderr, "7z encrypted header needs password")
    # ours -> py7zr
    for m in ["lzma2", "lzma", "bzip2", "deflate", "copy", "zstd"]:
        out = os.path.join(tmp, f"ours_{m}.7z")
        run("convert", os.path.join(tmp, "py_deflate.zip"), out, env={"OC_7Z_METHOD": m})
        if m == "zstd":
            check(only_files(listing(out)) == expected(), "7z zstd roundtrip"); continue
        try:
            d = os.path.join(tmp, f"x7_{m}")
            with py7zr.SevenZipFile(out) as z:
                z.extractall(d)
            ok = all(open(os.path.join(d, n), "rb").read() == b for n, b in FILES.items())
            check(ok, f"py7zr reads our 7z {m}")
        except Exception as ex:
            check(False, f"py7zr reads our 7z {m}: {ex}")
    out = os.path.join(tmp, "ours_aes.7z")
    run("convert", os.path.join(tmp, "py_deflate.zip"), out, env={"OC_OUT_PASSWORD": "pw9"})
    try:
        d = os.path.join(tmp, "x7_aes")
        with py7zr.SevenZipFile(out, password="pw9") as z:
            z.extractall(d)
        ok = all(open(os.path.join(d, n), "rb").read() == b for n, b in FILES.items())
        check(ok, "py7zr reads our encrypted 7z")
    except Exception as ex:
        check(False, f"py7zr reads our encrypted 7z: {ex}")
    check(only_files(listing(out, env={"OC_PASSWORD": "pw9"})) == expected(), "we read our encrypted 7z")

# --- ISO 9660 (pycdlib writes the images)
try:
    import pycdlib
except ImportError:
    pycdlib = None
if pycdlib:
    import re
    def isoify(p):
        return re.sub(r"[^A-Z0-9_]", "_", p.upper())[:30]
    for kind in ["plain", "joliet", "rockridge"]:
        iso = pycdlib.PyCdlib()
        kw = {"interchange_level": 4}
        if kind == "joliet": kw["joliet"] = 3
        if kind == "rockridge": kw["rock_ridge"] = "1.09"
        iso.new(**kw)
        files = FILES if kind != "plain" else {
            "HELLO.TXT": FILES["hello.txt"], "DIR/SUB/DATA.BIN": FILES["dir/sub/data.bin"], "BIG.TXT": FILES["big.txt"]}
        made = set()
        for n, b in files.items():
            parts = n.split("/")
            cur_iso = ""; cur_real = ""
            for d in parts[:-1]:
                cur_iso += "/" + isoify(d); cur_real += "/" + d
                if cur_iso not in made:
                    kwd = {}
                    if kind == "joliet": kwd["joliet_path"] = cur_real
                    if kind == "rockridge": kwd["rr_name"] = d
                    iso.add_directory(cur_iso, **kwd)
                    made.add(cur_iso)
            base, _, ext = parts[-1].rpartition(".") if "." in parts[-1] else (parts[-1], "", "")
            leaf = isoify(base) + ("." + isoify(ext) if ext else ".")
            kwf = {}
            if kind == "joliet": kwf["joliet_path"] = "/" + "/".join(parts)
            if kind == "rockridge": kwf["rr_name"] = parts[-1]
            iso.add_fp(io.BytesIO(b), len(b), cur_iso + "/" + leaf + ";1", **kwf)
        p = os.path.join(tmp, f"py_{kind}.iso")
        iso.write(p); iso.close()
        got = only_files(listing(p))
        want = {n: ("f", len(b), zlib.crc32(b)) for n, b in files.items()}
        if kind == "plain":
            check(sorted(got.values()) == sorted(want.values()), "read ISO 9660 (plain names)")
        else:
            check(got == want, f"read ISO 9660 ({kind}) long and unicode names")
sys.exit(1 if fail else 0)
