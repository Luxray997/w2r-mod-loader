"""Regenerate docs/vectors/index.json deterministically. Run: python3 tools/gen_index_vectors.py"""
import json
from pathlib import Path

from w2r_spec.samples import base_manifest

OUT = Path(__file__).resolve().parents[1] / "docs" / "vectors" / "index.json"
CACHED_SERIAL = 5


def entry(**changes):
    version = {
        "version": "1.2.0",
        "manifest": base_manifest(),
        "url": "https://example.com/example-plugin-1.2.0.zip",
        "sha256": "ab" * 32,
        "size": 4096,
        "min_manager": "0.1.0",
    }
    version.update(changes)
    return version


def index(**changes):
    doc = {
        "schema": 1,
        "serial": 7,
        "generated": "2026-10-02T00:00:00Z",
        "plugins": [{"id": "example-plugin", "versions": [entry()]}],
    }
    doc.update(changes)
    return doc


def text(doc):
    return json.dumps(doc, indent=2) + "\n"


def render(doc_or_text):
    return doc_or_text if isinstance(doc_or_text, str) else text(doc_or_text)


def plugins_with(**version_changes):
    return {"plugins": [{"id": "example-plugin", "versions": [entry(**version_changes)]}]}


def bad_manifest_doc():
    m = base_manifest()
    m["capabilities"] = ["net.recv"]
    return index(**plugins_with(manifest=m))


def drop(key):
    doc = index()
    del doc[key]
    return doc


def build_cases():
    cases = []

    def add(name, expect, body):
        cases.append({"name": name, "expect": expect, "index": body})

    add("valid", "ok", render(index()))
    add("valid_serial_equal_to_cached", "ok", render(index(serial=5)))

    add("not_json", "not_json", render("{"))
    add("lone_surrogate", "not_json", render('{"schema": 1, "x": "\\ud800"}'))
    add("depth_32_reaches_field_checks", "unknown_field",
        render('{"schema": 1, "x": ' + "[" * 31 + "]" * 31 + "}"))
    add("depth_33_refused", "not_json",
        render('{"schema": 1, "x": ' + "[" * 32 + "]" * 32 + "}"))
    add("serial_max_safe_integer", "ok", render(index(serial=2**53 - 1)))
    add("serial_too_large", "bad_type", render(index(serial=2**53)))
    add("hash_trailing_newline", "bad_hash", render(index(**plugins_with(sha256="ab" * 32 + "\n"))))
    add("min_manager_trailing_newline", "bad_version", render(index(**plugins_with(min_manager="0.1.0\n"))))
    add("plugin_id_device", "bad_id",
        render(index(plugins=[{"id": "con", "versions": [entry()]}])))
    add("duplicate_key", "duplicate_key", render('{"schema": 1, "schema": 1}'))
    add("not_object", "bad_type", render("[]"))
    add("unknown_top_field", "unknown_field", render(index(extra=1)))
    add("missing_serial", "missing_field", render(drop("serial")))
    add("old_expires_field", "unknown_field", render(index(expires="2026-11-02T00:00:00Z")))
    add("schema_2", "bad_schema", render(index(schema=2)))
    add("serial_negative", "bad_type", render(index(serial=-1)))
    add("serial_string", "bad_type", render(index(serial="7")))
    add("rollback", "rollback", render(index(serial=4)))
    add("time_bad_format", "bad_time", render(index(generated="2026-10-02 00:00:00")))
    add("time_bad_date", "bad_time", render(index(generated="2026-13-40T00:00:00Z")))
    add("plugins_not_list", "bad_type", render(index(plugins={})))

    add("url_http", "bad_url", render(index(**plugins_with(url="http://example.com/x.zip"))))
    add("url_userinfo", "bad_url", render(index(**plugins_with(url="https://u:p@example.com/x.zip"))))
    add("url_empty_user", "bad_url", render(index(**plugins_with(url="https://@example.com/x.zip"))))
    add("url_no_host", "bad_url", render(index(**plugins_with(url="https:///x.zip"))))
    add("url_space", "bad_url", render(index(**plugins_with(url="https://example.com/a b.zip"))))
    add("hash_uppercase", "bad_hash", render(index(**plugins_with(sha256="AB" * 32))))
    add("hash_short", "bad_hash", render(index(**plugins_with(sha256="ab" * 31))))
    add("size_zero", "bad_size", render(index(**plugins_with(size=0))))
    add("size_over_limit", "bad_size", render(index(**plugins_with(size=64 * 1024 * 1024 + 1))))
    add("size_string", "bad_type", render(index(**plugins_with(size="4096"))))
    add("min_manager_bad", "bad_version", render(index(**plugins_with(min_manager="x"))))
    add("version_field_bad", "bad_version", render(index(**plugins_with(version="1.2"))))
    add("version_field_mismatch", "mismatch", render(index(**plugins_with(version="1.3.0"))))
    add("bad_manifest", "bad_manifest", render(bad_manifest_doc()))
    add("entry_missing_field", "missing_field",
        render(index(plugins=[{"id": "example-plugin", "versions": [
            {k: v for k, v in entry().items() if k != "url"}]}])))
    add("plugin_id_mismatch", "mismatch",
        render(index(plugins=[{"id": "another-plugin", "versions": [entry()]}])))
    add("plugin_id_bad", "bad_id",
        render(index(plugins=[{"id": "Bad Id", "versions": [entry()]}])))
    add("duplicate_version", "duplicate",
        render(index(plugins=[{"id": "example-plugin", "versions": [entry(), entry()]}])))
    add("duplicate_plugin", "duplicate",
        render(index(plugins=[{"id": "example-plugin", "versions": [entry()]},
                              {"id": "example-plugin", "versions": [entry(version="1.3.0")]}])))
    add("empty_versions", "bad_type",
        render(index(plugins=[{"id": "example-plugin", "versions": []}])))
    return cases


def main():
    doc = {"cached_serial": CACHED_SERIAL, "cases": build_cases()}
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(doc, indent=1) + "\n", encoding="utf-8")
    print("wrote", OUT, len(doc["cases"]), "cases")


if __name__ == "__main__":
    main()
