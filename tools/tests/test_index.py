import json
import unittest
from datetime import datetime, timezone
from pathlib import Path

from w2r_spec import index, minisign

VECTORS = Path(__file__).resolve().parents[2] / "docs" / "vectors" / "index.json"
DOC = json.loads(VECTORS.read_text(encoding="utf-8"))
NOW = datetime.strptime(DOC["now"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
PRIV, PK, KID = minisign.make_keypair(bytes(range(32)))


def run(body, sig):
    return index.verify_index(body.encode("utf-8"), sig, DOC["public_key"], DOC["cached_serial"], NOW)


class IndexVectors(unittest.TestCase):
    def test_every_vector(self):
        for case in DOC["cases"]:
            with self.subTest(case["name"]):
                self.assertEqual(run(case["index"], case["signature"]), case["expect"])

    def test_names_unique(self):
        names = [c["name"] for c in DOC["cases"]]
        self.assertEqual(len(names), len(set(names)))

    def test_every_reason_code_has_a_vector_or_a_unit_test(self):
        # too_large and too_many are covered by the unit tests below, not by committed vectors.
        expected = {c["expect"] for c in DOC["cases"]}
        self.assertEqual(index.REASONS - expected - {"too_large", "too_many"}, set())


class IndexLimits(unittest.TestCase):
    def sign(self, body):
        return minisign.sign(body.encode(), PRIV, KID, "t")

    def test_oversize_index_is_refused_before_signature_work(self):
        body = " " * (index.MAX_INDEX_BYTES + 1)
        self.assertEqual(run(body, "ignored"), "too_large")

    def test_many_plugins_refused(self):
        valid = json.loads(DOC["cases"][0]["index"])
        valid["plugins"] = valid["plugins"] * (index.MAX_PLUGINS + 1)
        body = json.dumps(valid)
        self.assertEqual(run(body, self.sign(body)), "too_many")

    def test_many_versions_refused(self):
        valid = json.loads(DOC["cases"][0]["index"])
        valid["plugins"][0]["versions"] = valid["plugins"][0]["versions"] * (index.MAX_VERSIONS + 1)
        body = json.dumps(valid)
        self.assertEqual(run(body, self.sign(body)), "too_many")

    def test_runaway_nesting_is_refused(self):
        body = "[" * 20000 + "]" * 20000
        self.assertEqual(run(body, self.sign(body)), "not_json")


if __name__ == "__main__":
    unittest.main()
