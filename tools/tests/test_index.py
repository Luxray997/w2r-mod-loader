import json
import unittest
from pathlib import Path

from w2r_spec import index

VECTORS = Path(__file__).resolve().parents[2] / "docs" / "vectors" / "index.json"
DOC = json.loads(VECTORS.read_text(encoding="utf-8"))


def run(body):
    return index.check_index(body.encode("utf-8"), DOC["cached_serial"])


class IndexVectors(unittest.TestCase):
    def test_every_vector(self):
        for case in DOC["cases"]:
            with self.subTest(case["name"]):
                self.assertEqual(run(case["index"]), case["expect"])

    def test_names_unique(self):
        names = [c["name"] for c in DOC["cases"]]
        self.assertEqual(len(names), len(set(names)))

    def test_every_reason_code_has_a_vector_or_a_unit_test(self):
        # too_large and too_many are covered by the unit tests below, not by committed vectors.
        expected = {c["expect"] for c in DOC["cases"]}
        self.assertEqual(index.REASONS - expected - {"too_large", "too_many"}, set())


class IndexLimits(unittest.TestCase):
    def test_oversize_index_is_refused(self):
        self.assertEqual(run(" " * (index.MAX_INDEX_BYTES + 1)), "too_large")

    def test_many_plugins_refused(self):
        valid = json.loads(DOC["cases"][0]["index"])
        valid["plugins"] = valid["plugins"] * (index.MAX_PLUGINS + 1)
        self.assertEqual(run(json.dumps(valid)), "too_many")

    def test_many_versions_refused(self):
        valid = json.loads(DOC["cases"][0]["index"])
        valid["plugins"][0]["versions"] = valid["plugins"][0]["versions"] * (index.MAX_VERSIONS + 1)
        self.assertEqual(run(json.dumps(valid)), "too_many")

    def test_runaway_nesting_is_refused(self):
        self.assertEqual(run("[" * 20000 + "]" * 20000), "not_json")


if __name__ == "__main__":
    unittest.main()
