"""Turn unittest FAIL/ERROR blocks into GitHub Actions error annotations.

Annotations are visible without signing in, unlike raw job logs.
Usage: python scripts/ci_annotate.py test-output.txt
"""

import re
import sys
from pathlib import Path


def main(path: str) -> int:
    text = Path(path).read_text(encoding="utf-8", errors="replace")
    blocks = re.split(r"\n={20,}\n", text)
    count = 0
    for block in blocks:
        m = re.match(r"(FAIL|ERROR): (\S+) \(([^)]+)\)", block)
        if not m:
            continue
        body = block.split("\n" + "-" * 70, 1)[-1].strip()
        body = body.split("\n" + "-" * 70)[0]
        lines = body.splitlines()[-25:]
        msg = "%0A".join(line.replace("%", "%25").replace("\r", "") for line in lines)
        print(f"::error title={m.group(1)} {m.group(3)}.{m.group(2)}::{msg}")
        count += 1
        if count >= 10:
            break
    if count == 0:
        print("::error title=Tests failed::" + "%0A".join(text.splitlines()[-25:]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
