"""Minisign-format ed25519 signatures (algorithm "ED": BLAKE2b-512 prehash).

Written from the minisign file format description; no minisign binary was available to cross-check.
Task 5 records that interoperability with the real tool must be verified before the first release.
"""
import base64
import hashlib

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

ALG = b"ED"
LEGACY_ALG = b"Ed"
KEY_ALG = b"Ed"
TRUSTED_PREFIX = "trusted comment: "


def make_keypair(seed):
    priv = Ed25519PrivateKey.from_private_bytes(seed)
    pk = priv.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    return priv, pk, hashlib.sha256(pk).digest()[:8]


def public_key_text(pk, key_id):
    body = base64.b64encode(KEY_ALG + key_id + pk).decode()
    return "untrusted comment: minisign public key: %s\n%s\n" % (key_id[::-1].hex().upper(), body)


def sign(message, priv, key_id, trusted_comment, legacy=False):
    digest = message if legacy else hashlib.blake2b(message).digest()
    sig = priv.sign(digest)
    global_sig = priv.sign(sig + trusted_comment.encode("utf-8"))
    alg = LEGACY_ALG if legacy else ALG
    return (
        "untrusted comment: signature from w2rpm test key\n"
        + base64.b64encode(alg + key_id + sig).decode() + "\n"
        + TRUSTED_PREFIX + trusted_comment + "\n"
        + base64.b64encode(global_sig).decode() + "\n"
    )


def _parse_public(text):
    lines = text.split("\n")
    if len(lines) < 2 or not lines[0].startswith("untrusted comment:") or any(lines[2:]):
        raise ValueError("bad public key file")
    raw = base64.b64decode(lines[1], validate=True)
    if len(raw) != 42 or raw[:2] != KEY_ALG:
        raise ValueError("bad public key")
    return raw[2:10], raw[10:]


def verify(message, sig_text, pubkey_text):
    try:
        key_id, pk = _parse_public(pubkey_text)
        lines = sig_text.split("\n")
        if (len(lines) < 4 or not lines[0].startswith("untrusted comment:")
                or not lines[2].startswith(TRUSTED_PREFIX) or any(lines[4:])):
            return False
        raw = base64.b64decode(lines[1], validate=True)
        if len(raw) != 74 or raw[:2] != ALG or raw[2:10] != key_id:
            return False
        sig = raw[10:]
        global_sig = base64.b64decode(lines[3], validate=True)
        key = Ed25519PublicKey.from_public_bytes(pk)
        key.verify(sig, hashlib.blake2b(message).digest())
        key.verify(global_sig, sig + lines[2][len(TRUSTED_PREFIX):].encode("utf-8"))
        return True
    except (ValueError, InvalidSignature):
        return False
