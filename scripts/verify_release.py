"""Mechanical Definition-of-Done checks (spec §16). Exits non-zero on failure.

Usage:
    python scripts/verify_release.py          # full check (requires a clean git tree)
    python scripts/verify_release.py --ci     # everything except the git-clean check
"""

import argparse
import ast
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PKG = ROOT / "openconverter"
WEB = ROOT / "web"

FORBIDDEN = {"zlib", "gzip", "zipfile", "bz2", "lzma", "imghdr", "sndhdr", "cgi", "cgitb", "colorsys",
             "PIL", "imageio", "numpy", "magick", "wand", "ffmpeg"}
# URLs that may appear in shipped code. None are ever fetched:
#  - loopback addresses the server prints or compares against;
#  - the SVG XML namespace identifier (required by the DOM API, not a network request).
ALLOWED_URL_PREFIXES = ("http://127.0.0.1", "http://localhost", "http://[::1]", "http://{",
                        "http://www.w3.org/2000/svg")
REQUIRED_FILES = ["README.md", "LICENSE", "CHANGELOG.md", "pyproject.toml", ".gitignore", ".gitattributes",
                  "docs/ARCHITECTURE.md", "docs/FORMATS.md", "docs/API.md", "docs/PROGRESS.md",
                  ".github/workflows/ci.yml", "scripts/run.ps1", "scripts/make_fixtures.py",
                  "web/index.html", "web/css/app.css", "web/js/app.js", "web/js/api.js", "web/favicon.svg"]
REQUIRED_SCREENSHOTS = ["main-light.png", "main-dark.png", "queue-done.png", "history.png"]
MAX_SCREENSHOT = 500 * 1024
MAX_TRACKED = 2 * 1024 * 1024


def python_files():
    return sorted(PKG.rglob("*.py"))


def imported_modules(path: Path):
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                yield alias.name.split(".")[0], node.lineno
        elif isinstance(node, ast.ImportFrom):
            if node.level == 0 and node.module:
                yield node.module.split(".")[0], node.lineno


def check_stdlib_only():
    errors = []
    allowed = set(sys.stdlib_module_names) | {"openconverter"}
    for f in python_files():
        for mod, line in imported_modules(f):
            if mod not in allowed:
                errors.append(f"{f.relative_to(ROOT)}:{line}: non-stdlib import {mod!r}")
    return errors


def check_forbidden():
    errors = []
    for f in python_files():
        for mod, line in imported_modules(f):
            if mod in FORBIDDEN:
                errors.append(f"{f.relative_to(ROOT)}:{line}: forbidden module {mod!r}")
    return errors


URL_RE = re.compile(r"https?://[^\s'\"<>)]*")


def check_remote_urls():
    errors = []
    files = list(python_files()) + [p for p in WEB.rglob("*") if p.is_file()]
    for f in files:
        try:
            text = f.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for m in URL_RE.finditer(text):
            if not m.group(0).startswith(ALLOWED_URL_PREFIXES):
                line = text.count("\n", 0, m.start()) + 1
                errors.append(f"{f.relative_to(ROOT)}:{line}: remote URL {m.group(0)!r}")
    return errors


def check_inline_web():
    errors = []
    for f in WEB.rglob("*.html"):
        text = f.read_text(encoding="utf-8")
        rel = f.relative_to(ROOT)
        for m in re.finditer(r"<script\b([^>]*)>(.*?)</script>", text, re.S | re.I):
            if "src=" not in m.group(1) or m.group(2).strip():
                errors.append(f"{rel}: inline <script>")
        if re.search(r"<style\b", text, re.I):
            errors.append(f"{rel}: <style> element")
        if re.search(r"\sstyle\s*=", text, re.I):
            errors.append(f"{rel}: style= attribute")
        if re.search(r"\son[a-z]+\s*=", text, re.I):
            errors.append(f"{rel}: inline event handler (on*=)")
    for f in WEB.rglob("*.js"):
        text = f.read_text(encoding="utf-8")
        rel = f.relative_to(ROOT)
        if re.search(r"setAttribute\(\s*[\"']style[\"']", text):
            errors.append(f"{rel}: setAttribute('style', ...)")
        if re.search(r"\.(innerHTML|outerHTML)\s*=|insertAdjacentHTML|document\.write", text):
            errors.append(f"{rel}: HTML string injection API")
        if re.search(r"\beval\(|new Function\(", text):
            errors.append(f"{rel}: eval/new Function")
    return errors


def check_version():
    errors = []
    init = (PKG / "__init__.py").read_text(encoding="utf-8")
    m = re.search(r'__version__\s*=\s*"([^"]+)"', init)
    if not m:
        return ["openconverter/__init__.py: no __version__"]
    version = m.group(1)
    pyproject = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    if 'attr = "openconverter.__version__"' not in pyproject or re.search(r'^version\s*=\s*"', pyproject, re.M):
        errors.append("pyproject.toml must read the version dynamically from openconverter.__version__")
    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8") if (ROOT / "CHANGELOG.md").exists() else ""
    if f"## [{version}]" not in changelog:
        errors.append(f"CHANGELOG.md has no '## [{version}]' section")
    return errors


def check_required_files():
    errors = [f"missing {p}" for p in REQUIRED_FILES if not (ROOT / p).is_file()]
    shots = ROOT / "docs" / "screenshots"
    for name in REQUIRED_SCREENSHOTS:
        p = shots / name
        if not p.is_file():
            errors.append(f"missing docs/screenshots/{name}")
        elif p.stat().st_size >= MAX_SCREENSHOT:
            errors.append(f"docs/screenshots/{name} is {p.stat().st_size:,} bytes (limit {MAX_SCREENSHOT:,})")
    readme = (ROOT / "README.md").read_text(encoding="utf-8") if (ROOT / "README.md").exists() else ""
    for name in REQUIRED_SCREENSHOTS:
        if f"docs/screenshots/{name}" not in readme:
            errors.append(f"README.md does not reference docs/screenshots/{name}")
    return errors


def git(*args):
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, check=False)


def check_git_clean():
    out = git("status", "--porcelain")
    if out.returncode != 0:
        return ["git status failed: " + out.stderr.strip()]
    return [f"uncommitted: {line}" for line in out.stdout.splitlines() if line.strip()]


def check_tracked_sizes():
    out = git("ls-files", "-z")
    if out.returncode != 0:
        return ["git ls-files failed: " + out.stderr.strip()]
    errors = []
    for name in filter(None, out.stdout.split("\0")):
        p = ROOT / name
        if not p.is_file():
            continue
        if name.startswith("docs/screenshots/"):
            continue  # checked separately (< 500 KB each)
        if p.stat().st_size > MAX_TRACKED:
            errors.append(f"{name} is {p.stat().st_size:,} bytes (> 2 MB)")
    return errors


def check_test_files_tracked():
    """Every file under tests/ (fixtures included) must be committed, not ignored."""
    out = git("ls-files", "--others", "--ignored", "--exclude-standard", "--", "tests", "scripts", "web",
              "openconverter", "docs")
    if out.returncode != 0:
        return ["git ls-files failed: " + out.stderr.strip()]
    return [f"ignored by .gitignore but needed: {line}" for line in out.stdout.splitlines()
            if line.strip() and "__pycache__" not in line and not line.endswith(".pyc")]


CHECKS = [
    ("stdlib-only imports", check_stdlib_only),
    ("forbidden modules", check_forbidden),
    ("remote URLs", check_remote_urls),
    ("inline script/style", check_inline_web),
    ("version consistency", check_version),
    ("required docs and files", check_required_files),
    ("tracked file sizes", check_tracked_sizes),
    ("no needed files ignored", check_test_files_tracked),
]


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--ci", action="store_true", help="skip the git-clean check")
    args = ap.parse_args(argv)
    checks = list(CHECKS)
    if not args.ci:
        checks.append(("git status clean", check_git_clean))
    failed = False
    for name, fn in checks:
        errors = fn()
        status = "ok" if not errors else "FAIL"
        print(f"[{status:>4}] {name}")
        for e in errors:
            print(f"        {e}")
        failed = failed or bool(errors)
    print("verify_release: " + ("FAILED" if failed else "all checks passed"))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
