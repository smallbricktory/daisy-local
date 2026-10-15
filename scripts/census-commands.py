#!/usr/bin/env python3
"""Which registered Tauri commands are driven by a test, and which are not.

A command counts as covered when its own name, an `<cmd>_impl` /
`<cmd>_streaming_impl` variant, or the delegate the thin wrapper calls is
named under crates/tauri-app/tests/ or in an in-module `#[cfg(test)]` block.
Naming it is necessary, not sufficient -- this is a shortlist, not a verdict.

The delegate is read from the command's brace-matched body. A 1500-character
window was used before and ran off the end of short commands into whichever
function followed, which reported covered commands as bare (`delete_session`
-> `session_root`, `show_main_window` -> `hide`).
"""
import re, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
APP = ROOT / "crates/tauri-app"
main = (APP / "src/main.rs").read_text()

# The generate_handler![...] list.
m = re.search(r"generate_handler!\[(.*?)\]", main, re.S)
cmds = [c.strip() for c in m.group(1).split(",") if c.strip()]
cmds = [c.split("::")[-1] for c in cmds]

src = {}
for p in (APP / "src").rglob("*.rs"):
    src[p] = p.read_text()

def _body(t, after_sig):
    """The text between the function's own braces, starting from its signature."""
    start = t.find("{", after_sig)
    if start == -1:
        return ""
    depth, i = 0, start
    while i < len(t):
        if t[i] == "{":
            depth += 1
        elif t[i] == "}":
            depth -= 1
            if depth == 0:
                return t[start + 1:i]
        i += 1
    return t[start + 1:]


def delegate(name):
    """The first call inside `fn <name>(...)` that isn't a macro/builtin."""
    for p, t in src.items():
        mm = re.search(r"(?:pub )?(?:async )?fn " + re.escape(name) + r"\b", t)
        if not mm:
            continue
        body = _body(t, mm.end())
        for call in re.finditer(r"\b([a-z_][a-z0-9_]*)\s*\(", body):
            n = call.group(1)
            if n in ("fn", "if", "match", "for", "while", "return", "Ok", "Err",
                     "Some", "None", "format", "println", "eprintln", "vec",
                     "map_err", "unwrap", "expect", "into", "to_string", "clone",
                     "await", "ok_or", "and_then", "iter", "collect", "lock",
                     # wrappers that stand between the command and its delegate
                     "spawn_blocking", "ensure_licensed", "run_on_os_thread",
                     "block_in_place", "move", "get_webview_window", "state",
                     "emit", "expect_licensed", "spawn", "to_owned", "as_ref",
                     "get", "unwrap_or_default", "unwrap_or", "new"):
                continue
            return n, p
        # A delegate handed over by reference rather than called, as in
        # `spawn_blocking(path::to::thing_impl)`.
        ref = re.search(r"\b(\w+_impl)\b", body)
        if ref:
            return ref.group(1), p
    return None, None

tests = ""
for p in (APP / "tests").rglob("*.rs"):
    tests += p.read_text()

# In-module test blocks.
inmod = ""
for p, t in src.items():
    for mm in re.finditer(r"#\[cfg\(test\)\]\s*mod\s+\w+\s*\{", t):
        depth, i = 0, mm.end() - 1
        while i < len(t):
            if t[i] == "{":
                depth += 1
            elif t[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        inmod += t[mm.end():i]

rows = []
for c in cmds:
    d, p = delegate(c)
    key = d or c
    root = c[:-7] if c.endswith("_stream") else c
    names = [key, c, root + "_impl", root + "_streaming_impl"]
    where = []
    for hay, label in ((tests, "integration"), (inmod, "in-module")):
        hit = next((n for n in names if re.search(r"\b" + re.escape(n) + r"\b", hay)), None)
        if hit:
            where.append(f"{label}:{hit}")
    rows.append((c, key, (p.relative_to(APP).as_posix() if p else "?"), where))

untested = [r for r in rows if not r[3]]
print(f"{len(cmds)} commands registered; {len(untested)} with no test naming their delegate\n")
by_mod = {}
for c, k, p, w in untested:
    by_mod.setdefault(p, []).append((c, k))
for p in sorted(by_mod):
    print(f"--- {p}")
    for c, k in by_mod[p]:
        print(f"    {c:38s} -> {k}")
