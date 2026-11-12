#!/usr/bin/env python3
"""Write the demo tag taxonomy, contacts and session corpus into a profile."""
import json, os, sys, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cast import CAST, TAGS, ME
from build import SESSIONS, build

PROFILE = sys.argv[1]
HERO_PCM = sys.argv[2] if len(sys.argv) > 2 else None

BASE = 1787000000  # mid-2026; tags and contacts predate the recordings.

def ident(kind, name):
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"daisy-demo/{kind}/{name}"))

tag_ids, tags_doc = {}, []
for i, (name, (color, prompt, vocab)) in enumerate(TAGS.items()):
    tid = ident("tag", name)
    tag_ids[name] = tid
    used = sum(1 for s in SESSIONS if name in s["tags"])
    tags_doc.append({
        "id": tid, "name": name, "color_hex": color,
        "prompt_md": prompt, "vocab_md": vocab,
        "created_at_unix_seconds": BASE + i * 137,
        "use_count": used,
    })

contact_ids, contacts_doc = {}, []
for i, (name, (_role, emails)) in enumerate(CAST.items()):
    cid = ident("contact", name)
    contact_ids[name] = cid
    contacts_doc.append({
        "id": cid,
        "display_name": "Me" if name == ME else name,
        "emails": emails,
        "created_at_unix_seconds": BASE + i * 91,
        "updated_at_unix_seconds": BASE + i * 91,
    })
# The local speaker is stored under "Me", the way the app names its own row.
contact_ids["Me"] = contact_ids[ME]

with open(os.path.join(PROFILE, "tags.json"), "w") as f:
    json.dump({"schema_version": 1, "tags": tags_doc}, f, indent=1)
with open(os.path.join(PROFILE, "contacts.json"), "w") as f:
    json.dump({"schema_version": 1, "contacts": contacts_doc}, f, indent=1)

hero = open(HERO_PCM, "rb").read() if HERO_PCM and os.path.exists(HERO_PCM) else None
rows = build(PROFILE, tag_ids, contact_ids, hero_audio=hero)

print(f"{len(tags_doc)} tags, {len(contacts_doc)} contacts, {len(rows)} sessions")
for sid, title, dur, spk in rows:
    print(f"  {sid}  {dur//60:2d}m{dur%60:02d}s  {spk} speakers  {title}")
