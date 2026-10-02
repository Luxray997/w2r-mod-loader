"""Simulation fingerprint and peer tag (docs/load-policy.md, docs/abi.md)."""
import hashlib


def fingerprint(plugins):
    """4 bytes in wire order; all zero when `plugins` is empty. Input: enabled affects_simulation plugins."""
    ids = [p["id"] for p in plugins]
    if len(set(ids)) != len(ids):
        raise ValueError("duplicate plugin id")
    if not plugins:
        return b"\x00\x00\x00\x00"
    h = hashlib.sha256()
    for p in sorted(plugins, key=lambda p: p["id"].encode("utf-8")):
        h.update(p["id"].encode("utf-8") + b"\x00")
        h.update(p["version"].encode("utf-8") + b"\x00")
        h.update(bytes.fromhex(p["sha256"]))
    return h.digest()[:4]


def peer_tag(plugin_id):
    return hashlib.sha256(plugin_id.encode("utf-8")).digest()[:4]
