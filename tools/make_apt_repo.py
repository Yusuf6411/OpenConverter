#!/usr/bin/env python3
"""Build a signed APT repository from .deb files.

  tools/make_apt_repo.py <debs-dir> <repo-dir> --key <gpg-key-id> [--suite stable] [--gnupghome DIR]

Writes <repo-dir>/pool/main/*.deb, dists/<suite>/main/binary-<arch>/Packages(.gz), a Release
file with SHA256 sums, and signs it as InRelease (clear-signed) and Release.gpg (detached).
Needs dpkg-scanpackages (dpkg-dev) and gpg. Users add the public key to
/etc/apt/keyrings and a "signed-by" source line (see packaging/apt/README.md).
"""
import argparse
import datetime
import email.utils
import gzip
import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path


def sha256(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("debs")
    ap.add_argument("repo")
    ap.add_argument("--key", required=True)
    ap.add_argument("--suite", default="stable")
    ap.add_argument("--gnupghome")
    a = ap.parse_args()
    debs = sorted(Path(a.debs).glob("*.deb"))
    if not debs:
        print("no .deb files found", file=sys.stderr)
        return 1
    repo = Path(a.repo)
    pool = repo / "pool" / "main"
    pool.mkdir(parents=True, exist_ok=True)
    for d in debs:
        shutil.copy2(d, pool / d.name)
    arches = set()
    for d in debs:
        out = subprocess.run(["dpkg-deb", "-f", str(d), "Architecture"], capture_output=True, text=True, check=True)
        arches.add(out.stdout.strip())
    suite_dir = repo / "dists" / a.suite
    files = []
    for arch in sorted(arches):
        bdir = suite_dir / "main" / f"binary-{arch}"
        bdir.mkdir(parents=True, exist_ok=True)
        scan = subprocess.run(
            ["dpkg-scanpackages", "--arch", arch, "pool/main"],
            cwd=repo, capture_output=True, check=True,
        )
        (bdir / "Packages").write_bytes(scan.stdout)
        with gzip.GzipFile(bdir / "Packages.gz", "wb", mtime=0) as g:
            g.write(scan.stdout)
        for name in ("Packages", "Packages.gz"):
            files.append(bdir / name)
    now = email.utils.formatdate(usegmt=True)
    lines = [
        "Origin: OpenConverter",
        "Label: OpenConverter",
        f"Suite: {a.suite}",
        f"Codename: {a.suite}",
        f"Date: {now}",
        f"Architectures: {' '.join(sorted(arches))}",
        "Components: main",
        "Description: OpenConverter packages",
        "SHA256:",
    ]
    for f in files:
        rel = f.relative_to(suite_dir).as_posix()
        lines.append(f" {sha256(f)} {f.stat().st_size:>16} {rel}")
    release = suite_dir / "Release"
    release.write_text("\n".join(lines) + "\n")
    env = dict(os.environ)
    if a.gnupghome:
        env["GNUPGHOME"] = a.gnupghome
    base = ["gpg", "--batch", "--yes", "--pinentry-mode", "loopback", "--local-user", a.key]
    subprocess.run(base + ["--clearsign", "-o", str(suite_dir / "InRelease"), str(release)], check=True, env=env)
    subprocess.run(base + ["--armor", "--detach-sign", "-o", str(suite_dir / "Release.gpg"), str(release)], check=True, env=env)
    print(f"apt repository written to {repo} ({len(debs)} packages, arches: {', '.join(sorted(arches))})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
