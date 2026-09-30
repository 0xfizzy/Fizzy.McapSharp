"""Verify upstream provenance and the reviewed local storage patch fingerprint."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / "native/vendor/mcap"

def digest(path):
    data = path.read_bytes()
    return hashlib.sha256(data.replace(b"\r\n", b"\n") if b"\0" not in data else data).hexdigest()

def verify(ROOT):
    upstream = json.loads((ROOT / "UPSTREAM.json").read_text(encoding="utf-8"))
    patches = json.loads((ROOT / "PATCHES.json").read_text(encoding="utf-8"))["sha256"]
    expected = dict(upstream["files"], **patches)
    expected["LICENSE"] = upstream["license"]["sha256"]
    actual = {p.relative_to(ROOT).as_posix() for p in ROOT.rglob("*") if p.is_file()}
    if actual != set(expected) | {"UPSTREAM.json", "PATCHES.json"}:
        raise RuntimeError("Unexpected vendored files or missing upstream sources")
    for name, value in expected.items():
        if digest(ROOT / name) != value:
            raise RuntimeError(f"Unreviewed vendor change: {name}; review and update PATCHES.json")
    print(f"Vendored {upstream['crate']} {upstream['version']}: {len(patches)} reviewed patched/new files; license verified")

def main():
    for name in ("mcap", "zstd-sys"):
        verify(ROOT.parent / name)

if __name__ == "__main__":
    main()
