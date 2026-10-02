"""Reference verifier for index.json + index.json.sig (docs/index.md). Returns "ok" or a reason code."""
import base64
import json
import re
from datetime import datetime, timezone
from urllib.parse import urlsplit

from . import jsonutil, minisign
from .manifest import ID_RE, SEMVER_RE, validate_manifest

MAX_INDEX_BYTES = 8 * 1024 * 1024
MAX_PACKAGE_BYTES = 64 * 1024 * 1024
MAX_PLUGINS = 1000
MAX_VERSIONS = 200
TIME_FORMAT = "%Y-%m-%dT%H:%M:%SZ"
TIME_RE = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")

TOP_REQUIRED = {"schema", "serial", "generated", "expires", "plugins"}
TOP_OPTIONAL = {"next_key"}
PLUGIN_KEYS = {"id", "versions"}
VERSION_KEYS = {"version", "manifest", "url", "sha256", "size", "min_manager"}

REASONS = {
    "too_large", "bad_signature", "not_json", "duplicate_key", "bad_type", "unknown_field",
    "missing_field", "bad_schema", "bad_time", "rollback", "expired", "bad_key", "too_many",
    "bad_id", "duplicate", "bad_version", "bad_manifest", "mismatch", "bad_url", "bad_hash",
    "bad_size",
}


def _is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def _keys(obj, required, optional=frozenset()):
    if set(obj) - required - optional:
        return "unknown_field"
    if required - set(obj):
        return "missing_field"
    return None


def _time(s):
    if not isinstance(s, str) or not TIME_RE.match(s):
        raise jsonutil.Reject("bad_time")
    try:
        return datetime.strptime(s, TIME_FORMAT).replace(tzinfo=timezone.utc)
    except ValueError:
        raise jsonutil.Reject("bad_time") from None


def _url_ok(u):
    if not isinstance(u, str) or len(u) > 2048 or not u.isascii() or any(c <= " " for c in u):
        return False
    try:
        p = urlsplit(u)
        return p.scheme == "https" and bool(p.hostname) and p.username is None and p.password is None
    except ValueError:
        return False


def _key_ok(k):
    try:
        raw = base64.b64decode(k, validate=True)
    except (ValueError, TypeError):
        return False
    return len(raw) == 42 and raw[:2] == minisign.KEY_ALG


def _check_version(plugin_id, v, seen):
    if not isinstance(v, dict):
        return "bad_type"
    err = _keys(v, VERSION_KEYS)
    if err:
        return err
    if not isinstance(v["version"], str) or not SEMVER_RE.match(v["version"]):
        return "bad_version"
    if not isinstance(v["min_manager"], str) or not SEMVER_RE.match(v["min_manager"]):
        return "bad_version"
    if v["version"] in seen:
        return "duplicate"
    seen.add(v["version"])
    manifest = v["manifest"]
    if not isinstance(manifest, dict):
        return "bad_manifest"
    if validate_manifest(json.dumps(manifest).encode("utf-8")) != "ok":
        return "bad_manifest"
    if manifest["id"] != plugin_id or manifest["version"] != v["version"]:
        return "mismatch"
    if not _url_ok(v["url"]):
        return "bad_url"
    if not isinstance(v["sha256"], str) or not SHA256_RE.match(v["sha256"]):
        return "bad_hash"
    if not _is_int(v["size"]):
        return "bad_type"
    if not 1 <= v["size"] <= MAX_PACKAGE_BYTES:
        return "bad_size"
    return None


def _check_plugins(plugins):
    if not isinstance(plugins, list):
        return "bad_type"
    if len(plugins) > MAX_PLUGINS:
        return "too_many"
    seen_ids = set()
    for p in plugins:
        if not isinstance(p, dict):
            return "bad_type"
        err = _keys(p, PLUGIN_KEYS)
        if err:
            return err
        pid = p["id"]
        if not isinstance(pid, str) or not ID_RE.match(pid):
            return "bad_id"
        if pid in seen_ids:
            return "duplicate"
        seen_ids.add(pid)
        versions = p["versions"]
        if not isinstance(versions, list) or not versions:
            return "bad_type"
        if len(versions) > MAX_VERSIONS:
            return "too_many"
        seen = set()
        for v in versions:
            err = _check_version(pid, v, seen)
            if err:
                return err
    return None


def verify_index(index_raw, sig_text, pubkey_text, cached_serial, now):
    if len(index_raw) > MAX_INDEX_BYTES:
        return "too_large"
    if not minisign.verify(index_raw, sig_text, pubkey_text):
        return "bad_signature"
    try:
        doc = jsonutil.parse(index_raw)
        if not isinstance(doc, dict):
            return "bad_type"
        err = _keys(doc, TOP_REQUIRED, TOP_OPTIONAL)
        if err:
            return err
        if not _is_int(doc["schema"]) or doc["schema"] != 1:
            return "bad_schema"
        if not _is_int(doc["serial"]) or doc["serial"] < 0:
            return "bad_type"
        generated, expires = _time(doc["generated"]), _time(doc["expires"])
    except jsonutil.Reject as r:
        return r.code
    if generated > expires:
        return "bad_time"
    if doc["serial"] < cached_serial:
        return "rollback"
    if expires <= now:
        return "expired"
    if "next_key" in doc and not (isinstance(doc["next_key"], str) and _key_ok(doc["next_key"])):
        return "bad_key"
    return _check_plugins(doc["plugins"]) or "ok"
