#!/usr/bin/env python3
"""Clip-test report for one Daisy session folder.

Usage:
  clip-test-analyze.py SESSION_DIR [--ref REF] [--offset SEC | --window A:B] [--json]

SESSION_DIR holds telemetry.jsonl and live_transcript.jsonl (a recording's
session folder, or a clip_replay output folder). REF is the clip's reference
transcript: plain text, or TSV lines `start_ms<TAB>end_ms<TAB>text` (the eval
corpus format). Prints, per track and live engine:

  - first-interim lag p50/p90: first interim showing a committed word minus
    the wall time its audio end reached the engine
  - final lag p50/p90: final receive time minus the same
  - WER against REF, fillers (um, uh, umm, uhh, uhm, erm, hmm, mm) removed
    from both sides
  - decode cost per step, real-time factor and backlog max (Nemotron,
    whisper); result lag (SpeechAnalyzer)
  - process CPU mean/max (percent of one core), RSS peak, busiest threads
  - finalize stage durations

--window A:B (seconds of session time) limits words, lags and WER hypothesis
text to that span. --offset SEC with a timed REF sets the window to the
reference's span shifted by SEC, padded by 1 s before and 3 s after.

Standard library only.
"""
import argparse
import bisect
import json
import os
import re
import sys
import unicodedata

FILLERS = {"um", "uh", "umm", "uhh", "uhm", "erm", "hmm", "mm"}
TERMINAL_STAGES = {"done", "error", "awaiting-labels"}


def normalize(text):
    """Words of `text`: NFKC, lowercase, punctuation dropped (word-internal
    apostrophes kept), fillers removed."""
    text = unicodedata.normalize("NFKC", text).lower()
    text = re.sub(r"[^\w\s']", " ", text)
    text = re.sub(r"(?<!\w)'|'(?!\w)", " ", text)
    return [w for w in text.split() if w not in FILLERS]


def edit_ops(ref, hyp):
    """(substitutions, insertions, deletions) of the minimum word alignment."""
    n = len(hyp)
    cost = list(range(n + 1))
    sub = [0] * (n + 1)
    ins = list(range(n + 1))
    dele = [0] * (n + 1)
    for i in range(1, len(ref) + 1):
        pc, ps, pi, pd = cost, sub, ins, dele
        cost, sub, ins, dele = [i] + [0] * n, [0] + [0] * n, [0] + [0] * n, [i] + [0] * n
        r = ref[i - 1]
        for j in range(1, n + 1):
            if r == hyp[j - 1]:
                cost[j], sub[j], ins[j], dele[j] = pc[j - 1], ps[j - 1], pi[j - 1], pd[j - 1]
                continue
            c_sub, c_ins, c_del = pc[j - 1], cost[j - 1], pc[j]
            if c_sub <= c_ins and c_sub <= c_del:
                cost[j], sub[j], ins[j], dele[j] = c_sub + 1, ps[j - 1] + 1, pi[j - 1], pd[j - 1]
            elif c_ins <= c_del:
                cost[j], sub[j], ins[j], dele[j] = c_ins + 1, sub[j - 1], ins[j - 1] + 1, dele[j - 1]
            else:
                cost[j], sub[j], ins[j], dele[j] = c_del + 1, ps[j], pi[j], pd[j] + 1
    return sub[n], ins[n], dele[n]


def wer(ref_words, hyp_words):
    s, i, d = edit_ops(ref_words, hyp_words)
    return {
        "wer": round((s + i + d) / max(1, len(ref_words)), 4),
        "sub": s, "ins": i, "del": d,
        "ref_words": len(ref_words), "hyp_words": len(hyp_words),
    }


def load_ref(path):
    """(words, (first_start_ms, last_end_ms) or None)."""
    texts, spans = [], []
    with open(path, encoding="utf-8") as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) >= 3:
                try:
                    spans.append((int(float(parts[0])), int(float(parts[1]))))
                except ValueError:
                    pass
                texts.append(parts[2])
            else:
                texts.append(parts[0])
    span = (min(s for s, _ in spans), max(e for _, e in spans)) if spans else None
    return normalize(" ".join(texts)), span


def read_jsonl(path):
    out = []
    if not os.path.isfile(path):
        return out
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return out


def pct(values, p):
    """Nearest-rank percentile, `p` in 0..1."""
    if not values:
        return None
    v = sorted(values)
    return v[int(round((len(v) - 1) * p))]


def mean(values):
    return sum(values) / len(values) if values else None


class Anchors:
    """Audio time to wall time for one track, from its `anchor` records."""

    def __init__(self, points):
        points = sorted(points)
        self.audio = [a for a, _ in points]
        self.wall = [w for _, w in points]

    def wall_of(self, audio_ms):
        if not self.audio:
            return None
        i = bisect.bisect_right(self.audio, audio_ms) - 1
        i = max(i, 0)
        return self.wall[i] + (audio_ms - self.audio[i])


def in_window(window, start_ms, end_ms):
    if window is None:
        return True
    lo, hi = window
    return lo <= start_ms and end_ms <= hi


def analyze(session_dir, ref_path=None, offset_s=None, window_s=None):
    tel = read_jsonl(os.path.join(session_dir, "telemetry.jsonl"))
    live = read_jsonl(os.path.join(session_dir, "live_transcript.jsonl"))
    by_kind = {}
    for r in tel:
        by_kind.setdefault(r.get("kind"), []).append(r)

    ref_words, ref_span = (None, None)
    if ref_path:
        ref_words, ref_span = load_ref(ref_path)
    window = None
    if window_s is not None:
        window = (int(window_s[0] * 1000), int(window_s[1] * 1000))
    elif offset_s is not None and ref_span is not None:
        window = (int(offset_s * 1000) + ref_span[0] - 1000, int(offset_s * 1000) + ref_span[1] + 3000)

    report = {"session": os.path.basename(os.path.normpath(session_dir)), "window_ms": window}
    headers = by_kind.get("header", [])
    report["header"] = headers[0] if headers else None

    anchors = {}
    for a in by_kind.get("anchor", []):
        anchors.setdefault(a["track"], []).append((a["audio_ms"], a["wall_ms"]))
    anchors = {t: Anchors(p) for t, p in anchors.items()}

    captions = {}
    for rec in by_kind.get("words", []):
        track, engine = rec["track"], rec.get("engine", "")
        anc = anchors.get(track)
        c = captions.setdefault((track, engine), {"words": 0, "interim_lags": [], "final_lags": [], "unseen": 0})
        for text, start_ms, end_ms, first_seen in rec["words"]:
            if not in_window(window, start_ms, end_ms):
                continue
            c["words"] += 1
            end_wall = anc.wall_of(end_ms) if anc else None
            if end_wall is None:
                continue
            c["final_lags"].append(rec["wall_ms"] - end_wall)
            if first_seen is None:
                c["unseen"] += 1
            else:
                c["interim_lags"].append(first_seen - end_wall)
    report["captions"] = [
        {
            "track": t, "engine": e, "words": c["words"],
            "words_seen_in_interim": c["words"] - c["unseen"],
            "first_interim_lag_ms": {"p50": pct(c["interim_lags"], 0.5), "p90": pct(c["interim_lags"], 0.9)},
            "final_lag_ms": {"p50": pct(c["final_lags"], 0.5), "p90": pct(c["final_lags"], 0.9)},
        }
        for (t, e), c in sorted(captions.items())
    ]

    finals = [l for l in live if l.get("final") and l.get("kind", "final") == "final"]
    finals = [l for l in finals if window is None or window[0] <= l["start_ms"] <= window[1]]
    finals.sort(key=lambda l: (l["start_ms"], l["end_ms"]))
    report["accuracy"] = []
    if ref_words is not None:
        for track in ("mic", "system"):
            hyp = normalize(" ".join(l["text"] for l in finals if l["track"] == track))
            engines = sorted({l.get("engine", "") for l in finals if l["track"] == track})
            report["accuracy"].append({"source": f"live {track}", "engines": engines, **wer(ref_words, hyp)})
        hyp = normalize(" ".join(l["text"] for l in finals))
        report["accuracy"].append({"source": "live both tracks", "engines": sorted({l.get("engine", "") for l in finals}), **wer(ref_words, hyp)})

    decode = {}
    for d in by_kind.get("decode", []):
        if not in_window(window, d["decoded_ms"], d["decoded_ms"]):
            continue
        k = decode.setdefault((d["track"], d.get("engine", "")), {"cost": [], "step": 0, "backlog": []})
        k["cost"].extend(d["cost_ms"])
        k["step"] += d["step_ms"]
        k["backlog"].append(d["backlog_ms"])
    report["decode"] = [
        {
            "track": t, "engine": e, "steps": len(k["cost"]),
            "cost_ms": {"p50": pct(k["cost"], 0.5), "p90": pct(k["cost"], 0.9), "max": max(k["cost"]) if k["cost"] else None},
            "rtf": round(sum(k["cost"]) / k["step"], 4) if k["step"] else None,
            "backlog_max_ms": max(k["backlog"]) if k["backlog"] else None,
        }
        for (t, e), k in sorted(decode.items())
    ]

    results = {}
    for r in by_kind.get("result", []):
        if not in_window(window, r["audio_end_ms"], r["audio_end_ms"]):
            continue
        anc = anchors.get(r["track"])
        end_wall = anc.wall_of(r["audio_end_ms"]) if anc else None
        k = results.setdefault((r["track"], r.get("engine", ""), bool(r.get("final"))), {"lag": [], "lead": []})
        if end_wall is not None:
            k["lag"].append(r["wall_ms"] - end_wall)
        k["lead"].append(r["fed_ms"] - r["audio_end_ms"])
    report["results"] = [
        {
            "track": t, "engine": e, "final": f, "n": len(k["lead"]),
            "lag_ms": {"p50": pct(k["lag"], 0.5), "p90": pct(k["lag"], 0.9)},
            "fed_ahead_ms": {"p50": pct(k["lead"], 0.5), "p90": pct(k["lead"], 0.9)},
        }
        for (t, e, f), k in sorted(results.items())
    ]

    procs = by_kind.get("proc", [])
    threads = {}
    for p in procs:
        for name, cpu in p.get("thread_cpu", []):
            threads.setdefault(name, []).append(cpu)
    report["process"] = {
        "samples": len(procs),
        "cpu_pct": {"mean": round(mean([p["cpu_pct"] for p in procs]), 1) if procs else None,
                    "max": max((p["cpu_pct"] for p in procs), default=None)},
        "rss_peak_mb": max((p["rss_mb"] for p in procs), default=None),
        "threads_max": max((p["threads"] for p in procs if p.get("threads") is not None), default=None),
        "busiest_threads": sorted(
            ({"name": n, "cpu_pct_mean": round(sum(v) / max(1, len(procs)), 1)} for n, v in threads.items()),
            key=lambda x: -x["cpu_pct_mean"],
        )[:6],
    }

    runs, run = [], []
    for s in by_kind.get("stage", []):
        if run and s["stage"] == "finalizing" and run[-1]["stage"] != "finalizing":
            runs.append(run)
            run = []
        run.append(s)
    if run:
        runs.append(run)
    report["finalize"] = []
    for run in runs:
        stages = []
        for a, b in zip(run, run[1:] + [None]):
            if a["stage"] in TERMINAL_STAGES:
                stages.append({"stage": a["stage"], "ms": None})
            else:
                stages.append({"stage": a["stage"], "ms": (b["wall_ms"] - a["wall_ms"]) if b else None})
        report["finalize"].append({
            "total_ms": run[-1]["wall_ms"] - run[0]["wall_ms"],
            "stages": stages,
        })
    ends = by_kind.get("end", [])
    report["dropped_lines"] = sum(e.get("dropped", 0) for e in ends)
    return report


def fmt_s(ms):
    return "-" if ms is None else f"{ms / 1000:.2f}"


def print_report(r, out=None):
    w = (out or sys.stdout).write
    h = r["header"] or {}
    eng = h.get("engine") or {}
    w(f"session {r['session']}\n")
    if h:
        w(f"  app {h.get('app_version', '?')} ({h.get('build_sha', '?')}) · {h.get('os', '?')} {h.get('os_version', '')}".rstrip()
          + f" · {h.get('cpu_model', '?')} ({h.get('logical_cores', '?')} logical, {h.get('physical_cores', '?')} physical)\n")
        w(f"  live {h.get('live_mode', '?')}: {eng.get('engine', '-')} \"{eng.get('model', '')}\" · mic \"{h.get('mic_device', '')}\" · system \"{h.get('system_device', '')}\"\n")
    else:
        w("  no telemetry header\n")
    if r["window_ms"]:
        w(f"  window {r['window_ms'][0] / 1000:.1f}-{r['window_ms'][1] / 1000:.1f} s session time\n")

    w("\ncaption lag (s)                      words  in-interim  first-interim p50/p90  final p50/p90\n")
    for c in r["captions"]:
        fi, fl = c["first_interim_lag_ms"], c["final_lag_ms"]
        w(f"  {c['track']:<7} {c['engine']:<26} {c['words']:>6}  {c['words_seen_in_interim']:>10}"
          f"  {fmt_s(fi['p50']):>8} / {fmt_s(fi['p90']):<10}  {fmt_s(fl['p50'])} / {fmt_s(fl['p90'])}\n")
    if not r["captions"]:
        w("  no committed words in telemetry\n")

    if r["accuracy"]:
        w("\naccuracy (fillers removed)            WER    sub   ins   del   ref   hyp\n")
        for a in r["accuracy"]:
            label = f"{a['source']} [{', '.join(e for e in a['engines'] if e) or '-'}]"
            w(f"  {label:<34} {a['wer']:.3f} {a['sub']:>5} {a['ins']:>5} {a['del']:>5} {a['ref_words']:>5} {a['hyp_words']:>5}\n")

    if r["decode"]:
        w("\ndecode                               steps  cost p50/p90/max ms   RTF     backlog max ms\n")
        for d in r["decode"]:
            c = d["cost_ms"]
            w(f"  {d['track']:<7} {d['engine']:<26} {d['steps']:>6}  {c['p50']}/{c['p90']}/{c['max']:<12}"
              f" {d['rtf'] if d['rtf'] is not None else '-':<7} {d['backlog_max_ms']}\n")
    if r["results"]:
        w("\nresults                                     n  lag p50/p90 s   fed ahead p50/p90 s\n")
        for x in r["results"]:
            kind = "final" if x["final"] else "volatile"
            w(f"  {x['track']:<7} {x['engine']:<16} {kind:<9} {x['n']:>6}  {fmt_s(x['lag_ms']['p50'])} / {fmt_s(x['lag_ms']['p90'])}"
              f"   {fmt_s(x['fed_ahead_ms']['p50'])} / {fmt_s(x['fed_ahead_ms']['p90'])}\n")

    p = r["process"]
    w(f"\nprocess  samples {p['samples']} · cpu mean {p['cpu_pct']['mean']}% max {p['cpu_pct']['max']}% (of one core)"
      f" · rss peak {p['rss_peak_mb'] if p['rss_peak_mb'] is None else round(p['rss_peak_mb'])} MB · threads max {p['threads_max']}\n")
    for t in p["busiest_threads"]:
        w(f"  {t['name'] or '(unnamed)':<28} {t['cpu_pct_mean']:>6}% mean\n")

    for i, run in enumerate(r["finalize"], 1):
        w(f"\nfinalize run {i}: {fmt_s(run['total_ms'])} s\n")
        for s in run["stages"]:
            w(f"  {s['stage']:<18} {fmt_s(s['ms']) if s['ms'] is not None else ''}\n")
    if r["dropped_lines"]:
        w(f"\n{r['dropped_lines']} telemetry lines were dropped on a full queue\n")


def parse_window(text):
    a, b = text.split(":")
    return float(a), float(b)


def main(argv=None):
    ap = argparse.ArgumentParser(description="Clip-test report for one Daisy session folder.")
    ap.add_argument("session_dir")
    ap.add_argument("--ref", help="reference transcript: plain text or start_ms<TAB>end_ms<TAB>text lines")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--offset", type=float, help="session seconds at which the timed reference's zero plays")
    g.add_argument("--window", type=parse_window, help="A:B seconds of session time to score")
    ap.add_argument("--json", action="store_true", help="print the report as JSON")
    a = ap.parse_args(argv)
    if not os.path.isdir(a.session_dir):
        ap.error(f"{a.session_dir} is not a directory")
    r = analyze(a.session_dir, a.ref, a.offset, a.window)
    if a.json:
        json.dump(r, sys.stdout, indent=2)
        sys.stdout.write("\n")
    else:
        print_report(r)


if __name__ == "__main__":
    main()
