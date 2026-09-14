#!/usr/bin/env python3
"""Update version and per-target sha256 values in homebrew/agentsweep.rb.

Usage: update_homebrew_sha.py <version> <target>=<sha256> [<target>=<sha256> ...]
"""
import re
import sys
from pathlib import Path

FORMULA = Path(__file__).resolve().parent.parent / "homebrew" / "agentsweep.rb"


def main() -> None:
    if len(sys.argv) < 3:
        sys.exit(f"usage: {sys.argv[0]} <version> <target>=<sha256> [...]")

    version, pairs = sys.argv[1], sys.argv[2:]
    text = FORMULA.read_text()

    text, n = re.subn(r'version "[^"]+"', f'version "{version}"', text, count=1)
    if n != 1:
        sys.exit("could not find a version line to update")

    text, n = re.subn(r"/releases/download/v[^/]+/", f"/releases/download/v{version}/", text)
    if n == 0:
        sys.exit("could not find a releases/download URL to update")

    for pair in pairs:
        target, sha = pair.split("=", 1)
        pattern = re.compile(
            rf'(url "[^"]*agentsweep-{re.escape(target)}"\n\s*sha256 ")[^"]+(")'
        )
        text, n = pattern.subn(rf"\g<1>{sha}\g<2>", text)
        if n != 1:
            sys.exit(f"expected exactly one sha256 line for target '{target}', found {n}")

    FORMULA.write_text(text)


if __name__ == "__main__":
    main()
