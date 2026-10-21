#!/usr/bin/env python3
"""Write the Jordan Reyes demo corpus into a Daisy profile.

Emits, per session, exactly what a finalized desktop recording leaves on disk:
manifest, transcript (raw + dedup + markdown), summary (json + markdown),
chapters, notes, a live transcript log, the finalize sidecar, and meeting.opus.
Raw chunk wavs are not written - the profile reads as one where raw audio was
dropped after finalize.
"""
import hashlib, json, os, re, struct, subprocess, sys, uuid
from datetime import datetime
from zoneinfo import ZoneInfo

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cast import CAST, TAGS, ME
from sessions_a import SESSIONS_A
from sessions_b import SESSIONS_B
from sessions_c import SESSIONS_C
from sessions_hero import SESSIONS_HERO

SESSIONS = SESSIONS_A + SESSIONS_B + SESSIONS_C + SESSIONS_HERO
TZ = ZoneInfo("America/Vancouver")
RATE = 16000
CHUNK_SECONDS = 180
REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
WORK = os.environ.get("DAISY_DEMO_WORK", os.path.join(REPO, "target", "demo-corpus"))
HERO_TIMING = os.path.join(WORK, "hero_timing.json")


def jitter(key, lo, hi):
    """Deterministic pseudo-random in [lo, hi) from a string key."""
    h = int(hashlib.sha256(key.encode()).hexdigest()[:8], 16)
    return lo + (h % 10_000) / 10_000 * (hi - lo)


def split_sentences(text):
    """What a recognizer emits: one segment per sentence, not per turn."""
    parts = re.split(r"(?<=[.?!])\s+", text.strip())
    out = []
    for p in parts:
        # Very short fragments ride along with the sentence before them.
        if out and len(p.split()) <= 2:
            out[-1] = out[-1] + " " + p
        else:
            out.append(p)
    return out or [text]


def lay_out(lines, key):
    """Give every sentence a start and end in ms.

    Pace is 110-140 words a minute with a breath between sentences and a
    longer beat when the speaker changes - which is what puts a thirty-turn
    conversation at the length such a conversation actually runs.
    """
    out, t = [], 0
    for i, (who, text) in enumerate(lines):
        if i:
            t += int(jitter(f"{key}:{i}:turn", 600, 2600))
        for j, sent in enumerate(split_sentences(text)):
            if j:
                t += int(jitter(f"{key}:{i}:{j}:breath", 160, 520))
            words = len(sent.split())
            pace = jitter(f"{key}:{i}:{j}:pace", 0.43, 0.55)
            dur = int(max(700, words * pace * 1000))
            out.append((who, sent, t, t + dur))
            t += dur
    return out


def lay_out_measured(lines, turns):
    """Lay sentences inside turns whose spans were measured from real audio,
    dividing each turn by word count so a click on a line lands on the words."""
    out = []
    for (who, text), t in zip(lines, turns):
        sents = split_sentences(text)
        words = [max(1, len(x.split())) for x in sents]
        total = sum(words)
        span = t["end_ms"] - t["start_ms"]
        cursor = t["start_ms"]
        for sent, w in zip(sents, words):
            dur = int(span * w / total)
            out.append((who, sent, cursor, cursor + dur))
            cursor += dur
    return out


def hms(ms):
    s = ms // 1000
    return f"{s // 3600:02d}:{s % 3600 // 60:02d}:{s % 60:02d}"


def clusters_for(laid):
    """Cluster 1 is always the local speaker; remote voices take 0, 2, 3, ..."""
    ids, nxt = {"me": 1}, 0
    for who, *_ in laid:
        if who not in ids:
            ids[who] = nxt if nxt != 1 else 2
            nxt = ids[who] + 1
            if nxt == 1:
                nxt = 2
    return ids


def envelope_pcm(laid, total_ms, hero_pcm=None):
    """Interleaved stereo PCM16: L is the local mic, R is everyone else.

    Without real audio this is a low shaped hiss under the segments, so the
    waveform in the player follows the conversation instead of flatlining.
    """
    if hero_pcm is not None:
        return hero_pcm
    n = int(total_ms / 1000 * RATE)
    buf = bytearray(n * 4)
    state = 12345
    for who, _text, s, e in laid:
        i0, i1 = int(s / 1000 * RATE), min(n, int(e / 1000 * RATE))
        left = who == "me"
        for i in range(i0, i1):
            state = (1103515245 * state + 12345) & 0x7FFFFFFF
            # Syllable-rate amplitude wobble so the envelope reads as speech.
            k = (i - i0)
            wob = 0.45 + 0.55 * abs(((k // 900) % 2) - ((k % 900) / 900.0))
            v = int(((state % 2000) - 1000) * 0.5 * wob)
            off = i * 4 + (0 if left else 2)
            struct.pack_into("<h", buf, off, v)
    return bytes(buf)


def write_opus(pcm, out_path, tmp_dir):
    raw = os.path.join(tmp_dir, "pcm.raw")
    with open(raw, "wb") as f:
        f.write(pcm)
    subprocess.run(
        ["cargo", "run", "-q", "-p", "recording", "--example", "pcm_to_opus", "--", raw, out_path],
        check=True, capture_output=True, cwd=REPO)
    os.remove(raw)


def build(profile, tag_ids, contact_ids, hero_audio=None):
    sessions_dir = os.path.join(profile, "sessions")
    os.makedirs(sessions_dir, exist_ok=True)
    tmp_dir = WORK
    os.makedirs(tmp_dir, exist_ok=True)
    written = []

    for spec in SESSIONS:
        start_dt = datetime.strptime(spec["start"], "%Y-%m-%d %H:%M").replace(tzinfo=TZ)
        created = int(start_dt.timestamp())
        sid = f"daisy-{created}"
        root = os.path.join(sessions_dir, sid)
        os.makedirs(root, exist_ok=True)

        if spec.get("hero") and os.path.exists(HERO_TIMING):
            laid = lay_out_measured(spec["lines"], json.load(open(HERO_TIMING)))
        else:
            laid = lay_out(spec["lines"], spec["key"])
        total_ms = laid[-1][3] + 2400
        ids = clusters_for(laid)
        # The recorder carries the label the app gives its own speaker.
        names = {c: ("Me" if who == "me" else who) for who, c in ids.items()}

        # ---- transcript.json / transcript.dedup.json -----------------------
        chunks = []
        for ci in range(0, int(total_ms / 1000 // CHUNK_SECONDS) + 1):
            lo, hi = ci * CHUNK_SECONDS * 1000, (ci + 1) * CHUNK_SECONDS * 1000
            segs = [
                {"start_ms": s - lo, "end_ms": e - lo, "text": t,
                 "confidence": None, "speaker_id": ids[who]}
                for who, t, s, e in laid if lo <= s < hi
            ]
            if segs:
                chunks.append((ci + 1, segs))

        def transcript_doc(track):
            return {
                "schema_version": 1, "session_id": sid,
                "provider": "speechanalyzer_local", "model": "Apple SpeechAnalyzer",
                "transcribed_at_unix_seconds": created + int(total_ms / 1000) + 40,
                "chunks": [
                    {"chunk_index": ci, "tracks": [
                        {"track": track,
                         "source_wav_relative": f"chunks/{ci:04d}/{track}.wav",
                         "segments": segs}]}
                    for ci, segs in chunks
                ],
            }

        write_json(root, "transcript.json", transcript_doc("mic_aec"))
        write_json(root, "transcript.dedup.json", transcript_doc("mic_aec"))

        # ---- transcript.md -------------------------------------------------
        md = [f"# Transcript: {sid}", "_Provider: speechanalyzer_local (Apple SpeechAnalyzer)_", ""]
        for ci, segs in chunks:
            md.append(f"## Chunk {ci}")
            md.append("")
            last, line = None, []
            base = (ci - 1) * CHUNK_SECONDS * 1000
            for sg in segs:
                who = names[sg["speaker_id"]]
                if who != last:
                    if line:
                        md.append(" ".join(line))
                    line = [f"[{hms(base + sg['start_ms'])}] **{who}**: {sg['text']}"]
                    last = who
                else:
                    line.append(sg["text"])
            if line:
                md.append(" ".join(line))
            md.append("")
        write_text(root, "transcript.md", "\n".join(md))

        # ---- live_transcript.jsonl -----------------------------------------
        live = []
        for who, t, s, e in laid:
            live.append(json.dumps({
                "track": "mic" if who == "me" else "system",
                "start_ms": s, "end_ms": e, "text": t, "final": True,
                "received_at_unix": created + int(e / 1000) + 2, "kind": "final",
            }))
        write_text(root, "live_transcript.jsonl", "\n".join(live) + "\n")

        # ---- summary -------------------------------------------------------
        sm = spec["summary"]
        summary = {
            "v": 2, "session_id": sid,
            "generated_at_unix_seconds": created + int(total_ms / 1000) + 95,
            "produced_by": {"device": "demo-desktop", "provider": "lm_studio",
                            "model": "qwen3.5-4b-instruct"},
            "tldr": sm["tldr"],
            "action_items": [{"task": t, "owner": o} for t, o in sm["action_items"]],
            "decisions": sm["decisions"],
            "open_questions": sm["open_questions"],
            "key_topics": sm["key_topics"],
        }
        write_json(root, "summary.json", summary)

        lines_md = [f"# {spec['title']}", "", f"**TL;DR.** {sm['tldr']}", ""]
        if sm["action_items"]:
            lines_md += ["## Action items", ""]
            lines_md += [f"- {t} — _{o}_" for t, o in sm["action_items"]] + [""]
        if sm["decisions"]:
            lines_md += ["## Decisions", ""] + [f"- {d}" for d in sm["decisions"]] + [""]
        if sm["open_questions"]:
            lines_md += ["## Open questions", ""] + [f"- {q}" for q in sm["open_questions"]] + [""]
        lines_md += ["## Key topics", ""] + [f"- {k}" for k in sm["key_topics"]] + [""]
        write_text(root, "summary.md", "\n".join(lines_md))

        # ---- chapters ------------------------------------------------------
        chapters = []
        for idx, title, blurb in spec["chapters"]:
            chapters.append({"title": title, "start_hms": hms(laid[idx][2]), "summary": blurb})
        write_json(root, "chapters.json", {
            "schema_version": 1, "session_id": sid, "model": "qwen3.5-4b-instruct",
            "generated_at_unix_seconds": created + int(total_ms / 1000) + 120,
            "chapters": chapters,
        })

        if spec.get("notes"):
            write_text(root, "notes.md", spec["notes"] + "\n")
        write_text(root, "finalize.status.json", json.dumps(
            {"stage": "done", "progress": 1.0,
             "updated_at_unix": created + int(total_ms / 1000) + 130}))

        # ---- audio ---------------------------------------------------------
        pcm = envelope_pcm(laid, total_ms, hero_audio if spec.get("hero") else None)
        write_opus(pcm, os.path.join(root, "meeting.opus"), tmp_dir)

        # ---- manifest ------------------------------------------------------
        manifest_chunks = []
        for ci, segs in chunks:
            c_start = created + (ci - 1) * CHUNK_SECONDS
            c_dur = min(CHUNK_SECONDS, int(total_ms / 1000) - (ci - 1) * CHUNK_SECONDS)
            manifest_chunks.append({
                "index": ci,
                "started_at_unix_seconds": c_start,
                "ended_at_unix_seconds": c_start + c_dur,
                "duration_seconds": c_dur,
                "mic_wav_relative": f"chunks/{ci:04d}/mic.wav",
                "system_wav_relative": f"chunks/{ci:04d}/system.wav",
                "mic_aec_wav_relative": f"chunks/{ci:04d}/mic_aec.wav",
                "mic_dn_wav_relative": None,
                "mic_samples": c_dur * RATE,
                "system_samples": c_dur * RATE,
                "mic_voice_processed": False,
            })

        speaker_map = []
        for who, cid in sorted(ids.items(), key=lambda kv: kv[1]):
            display = names[cid]
            speaker_map.append({
                "cluster_id": cid, "display_name": display,
                "email": (CAST.get(ME if who == "me" else who, ("", []))[1] or [None])[0],
                "voiceprint_id": None, "match_confidence": None,
                "contact_id": contact_ids.get(display),
            })

        manifest = {
            "schema_version": 4, "session_id": sid,
            "created_at_unix_seconds": created,
            "sample_rate": RATE, "channels": 1,
            "mic_source_id": 71, "mic_source_node_name": "MacBook Pro Microphone",
            "mic_source_description": "MacBook Pro Microphone",
            "system_source_id": 3161626703, "system_source_node_name": "system-audio",
            "system_source_description": "System audio",
            "aec_mode": "always",
            "chunks": manifest_chunks,
            "finalized_at_unix_seconds": created + int(total_ms / 1000) + 130,
            "title": spec["title"],
            "meeting_id": str(uuid.uuid5(uuid.NAMESPACE_URL, "daisy-demo/" + spec["key"])),
            "tag_ids": [tag_ids[t] for t in spec["tags"]],
            "notes_md_relative": "notes.md" if spec.get("notes") else None,
            "attendees": ([{"display_name": ME, "role": "self"}] if spec["attendees"] else [])
                         + [{"display_name": a, "role": "other"} for a in spec["attendees"]],
            "calendar": None,
            "recording_segments": [{
                "started_at_unix_seconds": created,
                "stopped_at_unix_seconds": created + int(total_ms / 1000),
                "first_chunk_index": 1,
                "last_chunk_index": manifest_chunks[-1]["index"],
            }],
            "language": None, "interrupted": False, "denoise_applied": True,
            "speaker_map": speaker_map,
            "diarization_unavailable": False,
            "single_local_speaker": len(ids) == 1,
            "cluster_sides": [
                {"cluster_id": c, "side": "room" if w == "me" else "remote"}
                for w, c in sorted(ids.items(), key=lambda kv: kv[1])
            ],
            "mixed_clusters": [],
            "dismissed_suggested_tag_ids": [],
        }
        if spec.get("kind"):
            manifest["recording_kind"] = spec["kind"]
        write_json(root, "manifest.json", manifest)

        written.append((sid, spec["title"], int(total_ms / 1000), len(ids)))

    return written


def write_json(root, name, doc):
    with open(os.path.join(root, name), "w") as f:
        json.dump(doc, f, indent=1)


def write_text(root, name, text):
    with open(os.path.join(root, name), "w") as f:
        f.write(text)
