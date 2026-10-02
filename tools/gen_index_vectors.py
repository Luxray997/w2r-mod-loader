"""Regenerate docs/vectors/index.json deterministically. Run: python3 tools/gen_index_vectors.py

The signing keys are TEST-ONLY seeds committed on purpose. Never reuse them.
"""
import base64
import json
from pathlib import Path

from w2r_spec import minisign
from w2r_spec.samples import base_manifest

OUT = Path(__file__).resolve().parents[1] / "docs" / "vectors" / "index.json"
NOW = "2026-10-15T00:00:00Z"
CACHED_SERIAL = 5

PRIV, PK, KID = minisign.make_keypair(bytes(range(32)))
OTHER_PRIV, _, OTHER_KID = minisign.make_keypair(bytes(range(1, 33)))
PUBLIC = minisign.public_key_text(PK, KID)
NEXT_KEY = base64.b64encode(b"Ed" + OTHER_KID + minisign.make_keypair(bytes(range(1, 33)))[1]).decode()


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
        "expires": "2026-11-02T00:00:00Z",
        "plugins": [{"id": "example-plugin", "versions": [entry()]}],
    }
    doc.update(changes)
    return doc


def text(doc):
    return json.dumps(doc, indent=2) + "\n"


def signed(doc_or_text, priv=PRIV, kid=KID, legacy=False):
    body = doc_or_text if isinstance(doc_or_text, str) else text(doc_or_text)
    return body, minisign.sign(body.encode(), priv, kid, "timestamp:1790000000", legacy=legacy)


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

    def add(name, expect, pair):
        cases.append({"name": name, "expect": expect, "index": pair[0], "signature": pair[1]})

    add("valid", "ok", signed(index()))
    add("valid_serial_equal_to_cached", "ok", signed(index(serial=5)))
    add("valid_with_next_key", "ok", signed(index(next_key=NEXT_KEY)))

    body, sig = signed(index())
    add("tampered_body", "bad_signature", (body.replace('"serial": 7', '"serial": 8'), sig))
    add("wrong_key", "bad_signature", signed(index(), OTHER_PRIV, OTHER_KID))
    add("wrong_key_id", "bad_signature", signed(index(), PRIV, OTHER_KID))
    add("garbage_signature", "bad_signature", (body, "not a signature\n"))
    add("truncated_signature", "bad_signature", (body, "\n".join(sig.split("\n")[:3]) + "\n"))
    add("legacy_ed_algorithm", "bad_signature", signed(index(), legacy=True))
    add("trusted_comment_tampered", "bad_signature",
        (body, sig.replace("timestamp:1790000000", "timestamp:1790000001")))

    add("sig_crlf_accepted", "ok", (body, sig.replace("\n", "\r\n")))
    add("sig_trailing_blank_lines", "bad_signature", (body, sig + "\n\n"))
    add("sig_long_trusted_comment_ok", "ok",
        (body, minisign.sign(body.encode(), PRIV, KID, "c" * 3000)))
    add("sig_over_size_cap", "bad_signature",
        (body, minisign.sign(body.encode(), PRIV, KID, "c" * 5000)))
    add("not_json", "not_json", signed("{"))
    add("lone_surrogate", "not_json", signed('{"schema": 1, "x": "\\ud800"}'))
    add("depth_32_reaches_field_checks", "unknown_field",
        signed('{"schema": 1, "x": ' + "[" * 31 + "]" * 31 + "}"))
    add("depth_33_refused", "not_json",
        signed('{"schema": 1, "x": ' + "[" * 32 + "]" * 32 + "}"))
    add("serial_max_safe_integer", "ok", signed(index(serial=2**53 - 1)))
    add("serial_too_large", "bad_type", signed(index(serial=2**53)))
    add("hash_trailing_newline", "bad_hash", signed(index(**plugins_with(sha256="ab" * 32 + "\n"))))
    add("min_manager_trailing_newline", "bad_version", signed(index(**plugins_with(min_manager="0.1.0\n"))))
    add("plugin_id_device", "bad_id",
        signed(index(plugins=[{"id": "con", "versions": [entry()]}])))
    add("duplicate_key", "duplicate_key", signed('{"schema": 1, "schema": 1}'))
    add("not_object", "bad_type", signed("[]"))
    add("unknown_top_field", "unknown_field", signed(index(extra=1)))
    add("missing_serial", "missing_field", signed(drop("serial")))
    add("schema_2", "bad_schema", signed(index(schema=2)))
    add("serial_negative", "bad_type", signed(index(serial=-1)))
    add("serial_string", "bad_type", signed(index(serial="7")))
    add("rollback", "rollback", signed(index(serial=4)))
    add("expired", "expired", signed(index(expires="2026-10-10T00:00:00Z")))
    add("generated_after_expires", "bad_time", signed(index(generated="2026-11-03T00:00:00Z")))
    add("time_bad_format", "bad_time", signed(index(generated="2026-10-02 00:00:00")))
    add("time_bad_date", "bad_time", signed(index(expires="2026-13-40T00:00:00Z")))
    add("next_key_bad", "bad_key", signed(index(next_key="AAAA")))
    add("plugins_not_list", "bad_type", signed(index(plugins={})))

    add("url_http", "bad_url", signed(index(**plugins_with(url="http://example.com/x.zip"))))
    add("url_userinfo", "bad_url", signed(index(**plugins_with(url="https://u:p@example.com/x.zip"))))
    add("url_empty_user", "bad_url", signed(index(**plugins_with(url="https://@example.com/x.zip"))))
    add("url_no_host", "bad_url", signed(index(**plugins_with(url="https:///x.zip"))))
    add("url_space", "bad_url", signed(index(**plugins_with(url="https://example.com/a b.zip"))))
    add("hash_uppercase", "bad_hash", signed(index(**plugins_with(sha256="AB" * 32))))
    add("hash_short", "bad_hash", signed(index(**plugins_with(sha256="ab" * 31))))
    add("size_zero", "bad_size", signed(index(**plugins_with(size=0))))
    add("size_over_limit", "bad_size", signed(index(**plugins_with(size=64 * 1024 * 1024 + 1))))
    add("size_string", "bad_type", signed(index(**plugins_with(size="4096"))))
    add("min_manager_bad", "bad_version", signed(index(**plugins_with(min_manager="x"))))
    add("version_field_bad", "bad_version", signed(index(**plugins_with(version="1.2"))))
    add("version_field_mismatch", "mismatch", signed(index(**plugins_with(version="1.3.0"))))
    add("bad_manifest", "bad_manifest", signed(bad_manifest_doc()))
    add("entry_missing_field", "missing_field",
        signed(index(plugins=[{"id": "example-plugin", "versions": [
            {k: v for k, v in entry().items() if k != "url"}]}])))
    add("plugin_id_mismatch", "mismatch",
        signed(index(plugins=[{"id": "another-plugin", "versions": [entry()]}])))
    add("plugin_id_bad", "bad_id",
        signed(index(plugins=[{"id": "Bad Id", "versions": [entry()]}])))
    add("duplicate_version", "duplicate",
        signed(index(plugins=[{"id": "example-plugin", "versions": [entry(), entry()]}])))
    add("duplicate_plugin", "duplicate",
        signed(index(plugins=[{"id": "example-plugin", "versions": [entry()]},
                              {"id": "example-plugin", "versions": [entry(version="1.3.0")]}])))
    add("empty_versions", "bad_type",
        signed(index(plugins=[{"id": "example-plugin", "versions": []}])))
    return cases


def main():
    doc = {
        "public_key": PUBLIC,
        "now": NOW,
        "cached_serial": CACHED_SERIAL,
        "note": "TEST-ONLY keys: seed bytes(range(32)). Never reuse.",
        "cases": build_cases(),
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(doc, indent=1) + "\n", encoding="utf-8")
    print("wrote", OUT, len(doc["cases"]), "cases")


if __name__ == "__main__":
    main()
