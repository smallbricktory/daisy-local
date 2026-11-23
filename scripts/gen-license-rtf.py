#!/usr/bin/env python3
"""Generate LICENSE.rtf from LICENSE for installer license screens.

The macOS DMG license window renders a plain-text SLA resource as MacRoman
with a stale style table (stray mid-word bold, mojibake em-dashes); RTF
renders correctly. Run from the repo root after editing LICENSE:

    python3 scripts/gen-license-rtf.py

Markdown in LICENSE maps to RTF: `## X` -> bold heading, `**x**` -> bold,
`*x*` -> italic (may span lines). Hard line wraps are preserved.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def rtf_escape(text: str) -> str:
    out = []
    for ch in text:
        if ch in "\\{}":
            out.append("\\" + ch)
        elif ord(ch) > 127:
            out.append(f"\\u{ord(ch)}?")
        else:
            out.append(ch)
    return "".join(out)


def main() -> int:
    src = (ROOT / "LICENSE").read_text(encoding="utf-8")

    # Inline styles on the escaped text; escaping never produces * so the
    # markers survive escaping intact.
    text = rtf_escape(src)
    text = re.sub(r"\*\*(.+?)\*\*", r"{\\b \1}", text, flags=re.S)
    text = re.sub(r"(?<!\*)\*([^*].*?)\*(?!\*)", r"{\\i \1}", text, flags=re.S)

    lines = []
    for line in text.split("\n"):
        m = re.match(r"^## (.*)$", line)
        if m:
            lines.append("{\\b\\fs26 " + m.group(1) + "}\\par")
        else:
            lines.append(line + "\\par")

    body = "\n".join(lines)
    rtf = (
        "{\\rtf1\\ansi\\deff0\\uc1\n"
        "{\\fonttbl{\\f0\\fswiss Helvetica;}}\n"
        "\\f0\\fs24\n" + body + "\n}\n"
    )
    (ROOT / "LICENSE.rtf").write_text(rtf, encoding="ascii")
    print(f"wrote {ROOT / 'LICENSE.rtf'} ({len(rtf)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
