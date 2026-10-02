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


MAX_DEPTH = 32  # containers nested deeper than this are refused (the root object is depth 1)


def _check(doc):
    """Refuse lone surrogates in any string or key, and nesting deeper than MAX_DEPTH."""
    stack = [(doc, 1)]
    while stack:
        node, depth = stack.pop()
        if isinstance(node, str):
            try:
                node.encode("utf-8")
            except UnicodeEncodeError:
                raise Reject("not_json") from None
        elif isinstance(node, (dict, list)):
            if depth > MAX_DEPTH:
                raise Reject("not_json")
            if isinstance(node, dict):
                stack.extend((k, depth + 1) for k in node)
                stack.extend((v, depth + 1) for v in node.values())
            else:
                stack.extend((v, depth + 1) for v in node)


def parse(raw):
    """Parse UTF-8 JSON bytes. Refuses duplicate keys, NaN/Infinity, a BOM, lone surrogates and
    nesting deeper than MAX_DEPTH. The first problem met while parsing, in document order, wins;
    the surrogate and depth checks run after a successful parse."""
    try:
        doc = json.loads(
            raw.decode("utf-8"), object_pairs_hook=_pairs, parse_constant=_constant
        )
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise Reject("not_json") from None
    _check(doc)
    return doc
