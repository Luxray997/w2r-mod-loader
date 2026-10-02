import json
import unittest
from pathlib import Path

from w2r_spec import manifest
from w2r_spec.samples import base_manifest

VECTORS = Path(__file__).resolve().parents[2] / "docs" / "vectors" / "manifest.json"


def raw_of(case):
    if "raw" in case:
        return case["raw"].encode("utf-8") + b" " * case.get("pad", 0)
    return json.dumps(case["manifest"]).encode("utf-8")


class ManifestVectors(unittest.TestCase):
    cases = json.loads(VECTORS.read_text(encoding="utf-8"))

    def test_every_vector(self):
        for case in self.cases:
            with self.subTest(case["name"]):
                self.assertEqual(manifest.validate_manifest(raw_of(case)), case["expect"])

    def test_names_unique(self):
        names = [c["name"] for c in self.cases]
        self.assertEqual(len(names), len(set(names)))

    def test_every_reason_code_has_a_vector(self):
        expected = {c["expect"] for c in self.cases}
        self.assertEqual(manifest.REASONS - expected, set())

    def test_runaway_nesting_is_refused(self):
        raw = b"[" * 20000 + b"]" * 20000
        self.assertEqual(manifest.validate_manifest(raw), "not_json")

    def test_valid_example_matches_layout_doc(self):
        self.assertEqual(manifest.validate_manifest(json.dumps(base_manifest()).encode()), "ok")


if __name__ == "__main__":
    unittest.main()
