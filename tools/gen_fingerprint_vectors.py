"""Regenerate docs/vectors/fingerprint.json. Run: python3 tools/gen_fingerprint_vectors.py"""
import json
from pathlib import Path

from w2r_spec.fingerprint import fingerprint, peer_tag

OUT = Path(__file__).resolve().parents[1] / "docs" / "vectors" / "fingerprint.json"

A = {"id": "example-plugin", "version": "1.2.0", "sha256": "11" * 32}
B = {"id": "another-plugin", "version": "0.3.1", "sha256": "22" * 32}


def main():
    doc = {
        "fingerprints": [
            {"name": "empty_is_zero", "plugins": [], "expect": "00000000"},
            {"name": "one_plugin", "plugins": [A], "expect": fingerprint([A]).hex()},
            {"name": "two_plugins", "plugins": [A, B], "expect": fingerprint([A, B]).hex()},
            {"name": "two_plugins_reversed", "plugins": [B, A], "expect": fingerprint([A, B]).hex()},
            {"name": "version_changes_it", "plugins": [dict(A, version="1.2.1")],
             "expect": fingerprint([dict(A, version="1.2.1")]).hex()},
        ],
        "duplicate_ids_refused": [A, dict(A, version="9.9.9")],
        "tags": [
            {"id": "example-plugin", "expect": peer_tag("example-plugin").hex()},
            {"id": "another-plugin", "expect": peer_tag("another-plugin").hex()},
        ],
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(doc, indent=1) + "\n", encoding="utf-8")
    print("wrote", OUT)


if __name__ == "__main__":
    main()
