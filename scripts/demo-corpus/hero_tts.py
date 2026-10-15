#!/usr/bin/env python3
"""Synthesize the hero session's audio, one request per speaking turn.

Writes hero_pcm.raw (interleaved stereo PCM16 at 16 kHz, left = the local mic,
right = everyone else) and hero_timing.json (the millisecond span of each turn,
measured from the audio that came back rather than guessed).
"""
import json, os, struct, sys, urllib.request
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from sessions_hero import SESSIONS_HERO

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = os.environ.get("DAISY_DEMO_WORK", os.path.join(REPO, "target", "demo-corpus"))
RATE = 16000
MODEL = "gpt-4o-mini-tts"
VOICE = {
    "me": "sage",
    "Priya Raman": "coral",
    "Simone Okafor": "alloy",
    "Devon Clarke": "ash",
}
TONE = ("Read this as one turn in a relaxed internal design critique at a small "
        "software company. Conversational, unhurried, a little dry. Do not "
        "announce or perform; just talk.")

key = os.environ["OPENAI_API_KEY"]
spec = SESSIONS_HERO[0]


def speak(item):
    i, (who, text) = item
    body = json.dumps({
        "model": MODEL, "voice": VOICE[who], "input": text,
        "instructions": TONE, "response_format": "wav",
    }).encode()
    req = urllib.request.Request(
        "https://api.openai.com/v1/audio/speech", data=body,
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as r:
        wav = r.read()
    return i, who, text, wav


def pcm_from_wav(b):
    """Mono PCM16 at 16 kHz from a RIFF/WAVE payload, resampled if needed."""
    assert b[:4] == b"RIFF" and b[8:12] == b"WAVE", "not a wav"
    pos, fmt, data = 12, None, None
    while pos + 8 <= len(b):
        cid, size = b[pos:pos + 4], struct.unpack_from("<I", b, pos + 4)[0]
        chunk = b[pos + 8:pos + 8 + size]
        if cid == b"fmt ":
            fmt = struct.unpack_from("<HHIIHH", chunk, 0)
        elif cid == b"data":
            data = chunk
        pos += 8 + size + (size & 1)
    _tag, ch, sr, _br, _al, bits = fmt
    assert bits == 16, f"expected 16-bit, got {bits}"
    samples = list(struct.unpack_from(f"<{len(data) // 2}h", data, 0))
    if ch > 1:
        samples = [sum(samples[i:i + ch]) // ch for i in range(0, len(samples), ch)]
    if sr != RATE:
        n = int(len(samples) * RATE / sr)
        out = []
        for i in range(n):
            x = i * sr / RATE
            j = int(x)
            f = x - j
            a = samples[j]
            bb = samples[min(j + 1, len(samples) - 1)]
            out.append(int(a + (bb - a) * f))
        samples = out
    return samples


def main():
    lines = spec["lines"]
    with ThreadPoolExecutor(max_workers=4) as ex:
        results = list(ex.map(speak, enumerate(lines)))
    results.sort(key=lambda r: r[0])

    rendered = []
    for i, who, text, wav in results:
        rendered.append((who, text, pcm_from_wav(wav)))
        print(f"  turn {i + 1:2d}/{len(lines)}  {who:<14} {len(rendered[-1][2]) / RATE:5.1f}s")

    # Lay the turns end to end with a beat between speakers.
    timing, cursor, total = [], 0, 0
    gaps = []
    for idx, (who, text, pcm) in enumerate(rendered):
        gap = 0 if idx == 0 else (700 if rendered[idx - 1][0] != who else 350)
        cursor += gap
        gaps.append(gap)
        dur_ms = int(len(pcm) / RATE * 1000)
        timing.append({"start_ms": cursor, "end_ms": cursor + dur_ms})
        cursor += dur_ms
    total = cursor + 2000

    n = int(total / 1000 * RATE)
    buf = bytearray(n * 4)
    for (who, _text, pcm), t in zip(rendered, timing):
        off0 = int(t["start_ms"] / 1000 * RATE)
        side = 0 if who == "me" else 2
        for k, v in enumerate(pcm):
            i = off0 + k
            if i >= n:
                break
            struct.pack_into("<h", buf, i * 4 + side, v)

    os.makedirs(OUT, exist_ok=True)
    with open(f"{OUT}/hero_pcm.raw", "wb") as f:
        f.write(bytes(buf))
    with open(f"{OUT}/hero_timing.json", "w") as f:
        json.dump(timing, f)
    print(f"hero audio: {total / 1000:.1f}s, {len(rendered)} turns")


main()
