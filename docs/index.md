# Plugin index (`index.json`)

The index lists installable plugins. It is fetched only by the manager (the in-game host never
touches the network) and cached as `cache\index.json` plus `cache\index.json.sig` (see
[layout.md](layout.md)). The rules are normative; `tools/w2r_spec/index.py` and
`tools/w2r_spec/minisign.py` are the executable reference and `docs/vectors/index.json` holds the
test vectors.

## Signature

- Format: minisign file format, ed25519, algorithm `ED`. The signed message is the BLAKE2b-512
  digest of the raw bytes of `index.json`. The legacy `Ed` mode (no prehash) is refused.
- A signature file has four lines: `untrusted comment: ...`, base64 of `"ED" || key id (8) ||
  signature (64)`, `trusted comment: ...`, base64 of the global signature over `signature ||
  trusted comment`. Both signatures must verify, and the key id must match the embedded public key.
  Extra non-empty lines are refused.
- Order of work: check the size limit, verify the signature over the exact received bytes, and only
  then parse the JSON. Nothing in an unsigned index is ever interpreted.
- **Interoperability is not yet verified.** The reference implementation was written from the
  minisign format description and no `minisign` binary was available. The index signature must be
  checked against the real tool before the first release (tracked in Phase 6).

## Document

```json
{
  "schema": 1,
  "serial": 7,
  "generated": "2026-10-02T00:00:00Z",
  "expires": "2026-11-02T00:00:00Z",
  "next_key": null,
  "plugins": [
    {
      "id": "example-plugin",
      "versions": [
        {
          "version": "1.2.0",
          "manifest": { "...": "a complete plugin.json, see manifest.md" },
          "url": "https://example.com/example-plugin-1.2.0.zip",
          "sha256": "<64 lowercase hex characters of the package>",
          "size": 4096,
          "min_manager": "0.1.0"
        }
      ]
    }
  ]
}
```

`next_key` is optional; when present it is a base64 minisign public key body. (The example shows
`null` for illustration only; the validator requires a valid key string when the field is present,
so omit the field when no rotation is planned.)

## Rules and reason codes

| Check | Rule | Code |
|---|---|---|
| size | index at most 8 MiB (8,388,608 bytes), checked first | `too_large` |
| signature | see above | `bad_signature` |
| JSON | strict rules from [manifest.md](manifest.md) (no duplicate keys, `NaN`, BOM, runaway nesting) | `not_json`, `duplicate_key` |
| top level | an object; exactly the fields above (`next_key` optional) | `bad_type`, `unknown_field`, `missing_field` |
| `schema` | integer 1 | `bad_schema` |
| `serial` | integer, 0 or more | `bad_type` |
| `generated`, `expires` | exactly `YYYY-MM-DDTHH:MM:SSZ`, a real calendar date and time, UTC; `generated` <= `expires` | `bad_time` |
| freshness | `serial` not lower than the cached serial (equal is accepted); `expires` later than now | `rollback`, `expired` |
| `next_key` | base64 of 42 bytes starting `Ed` | `bad_key` |
| `plugins` | list, at most 1,000; each entry exactly `id`, `versions`; `id` follows the manifest id rule and is unique | `bad_type`, `too_many`, `bad_id`, `duplicate` |
| `versions` | non-empty list, at most 200 per plugin; `version` and `min_manager` are SemVer; `(id, version)` unique | `bad_type`, `too_many`, `bad_version`, `duplicate` |
| `manifest` | passes [manifest.md](manifest.md); its `id` and `version` equal the entry's | `bad_manifest`, `mismatch` |
| `url` | ASCII, at most 2,048 characters, no whitespace, scheme `https`, a host, no userinfo (even an empty one) | `bad_url` |
| `sha256` | 64 lowercase hex characters | `bad_hash` |
| `size` | integer from 1 to 67,108,864 (64 MiB) | `bad_size`, `bad_type` |

The first failing check wins, in the order of this table. The manager refuses the whole index on any
failure and keeps using the previously cached one.

## Freshness and rollback

- The manager stores the highest `serial` it has accepted. A lower serial is refused so that a
  network attacker cannot replay an older index to hide an update or reinstate a vulnerable
  version. An equal serial is accepted so that re-fetching an unchanged index works.
- An index whose `expires` has passed is refused. A wrong system clock is a likely cause; the UI
  says which check failed and shows the clock time it used.
- Publishers raise `serial` on every publication and set `expires` to a limited horizon (suggested:
  30 days) and re-sign before it.

## Package verification (used by the manager, Phase 4)

1. Download only the `url` from the verified index entry.
2. Compare the package's SHA-256 and size with the index entry.
3. Extract with the archive rules below, then parse the packaged `plugin.json` and compare it to the
   index copy; refuse on any difference.
4. Install atomically (staging directory, then rename).

Archive rules (the extractor stops at the first violation and deletes its staging directory):

- no absolute paths, drive letters, UNC forms or `..` components; no symlinks or reparse points;
- only `plugin.json`, the file named by the manifest `entry`, and an optional `README.md` and
  `LICENSE`; any other name is refused;
- at most 64 entries, at most 128 MiB uncompressed in total, and a per-file compression ratio
  limit of 100:1;
- entry names are compared after normalisation and must not collide case-insensitively.

## Key management

- The manager embeds the release public key. The signing key is kept offline.
- Rotation: publish an index that carries `next_key` signed by the current key, ship a manager
  release embedding the new key, then switch signing to the new key. Managers older than that
  release must be updated to continue.
- A compromised signing key breaks the model and requires a new manager release. The threat model
  says so.

## Test vectors

`docs/vectors/index.json` has the public key, a fixed "now" (`2026-10-15T00:00:00Z`), the cached
serial (5) and a list of `{name, expect, index, signature}` cases. The signing keys are **test
only**: the seed is `bytes(range(32))` and is committed on purpose. Never reuse it. Oversize and
too-many-entries cases are covered by unit tests rather than committed vectors (they would be
megabytes). Regenerate with `python3 tools/gen_index_vectors.py`; run with `bash tools/run_all.sh`.
