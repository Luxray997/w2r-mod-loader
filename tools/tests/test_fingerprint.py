import json
import unittest
from pathlib import Path

from w2r_spec import fingerprint as fp

VECTORS = Path(__file__).resolve().parents[2] / "docs" / "vectors" / "fingerprint.json"
DOC = json.loads(VECTORS.read_text(encoding="utf-8")) if VECTORS.exists() else {}


class FingerprintVectors(unittest.TestCase):
    def test_fingerprints(self):
        for case in DOC["fingerprints"]:
            with self.subTest(case["name"]):
                self.assertEqual(fp.fingerprint(case["plugins"]).hex(), case["expect"])

    def test_order_independent(self):
        by_name = {c["name"]: c["expect"] for c in DOC["fingerprints"]}
        self.assertEqual(by_name["two_plugins"], by_name["two_plugins_reversed"])
        self.assertNotEqual(by_name["one_plugin"], by_name["version_changes_it"])

    def test_duplicate_ids_refused(self):
        with self.assertRaises(ValueError):
            fp.fingerprint(DOC["duplicate_ids_refused"])

    def test_tags(self):
        for case in DOC["tags"]:
            with self.subTest(case["id"]):
                self.assertEqual(fp.peer_tag(case["id"]).hex(), case["expect"])

    def test_colliding_tags_are_detectable(self):
        # The load policy refuses the later of two plugins with equal tags; this is the check it uses.
        tags = [fp.peer_tag("example-plugin"), fp.peer_tag("another-plugin")]
        self.assertEqual(len(set(tags)), 2)
        self.assertEqual(len(fp.peer_tag("example-plugin")), 4)


if __name__ == "__main__":
    unittest.main()
