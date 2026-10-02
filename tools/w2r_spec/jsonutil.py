"""Strict JSON parsing shared by the manifest and index reference validators."""
import json


class Reject(Exception):
    def __init__(self, code):
        super().__init__(code)
        self.code = code


def _pairs(pairs):
    out = {}
    for key, value in pairs:
        if key in out:
            raise Reject("duplicate_key")
        out[key] = value
    return out


def _constant(name):
    raise Reject("not_json")


def parse(raw):
    """Parse UTF-8 JSON bytes. Refuses duplicate keys, NaN/Infinity, a BOM and runaway nesting."""
    try:
        return json.loads(
            raw.decode("utf-8"), object_pairs_hook=_pairs, parse_constant=_constant
        )
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise Reject("not_json") from None
