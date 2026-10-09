# Plugin index (`index.json`)

The index lists installable plugins. It is fetched only by the manager (the in-game host never
touches the network) and cached as `cache\index.json` (see [layout.md](layout.md)). The rules are
normative; `tools/w2r_spec/index.py` is the executable reference and `docs/vectors/index.json` holds
the test vectors.

## Trust

The index is **not signed**. This is a small hobby project and the maintainers decided that
offline signing keys and a signing step on every release are not worth it. What protects a user:

- the index and every package are fetched over HTTPS only (also across redirects);
- the index carries the SHA-256 and size of each package, and the manager checks both before it
  opens the archive;
- the packaged `plugin.json` must equal the index copy.

What it does not protect against: whoever controls the index host (the `w2r-plugins` GitHub
account, its Pages site or its release assets) can publish any plugin. Plugins are native code, so
users should only install plugins from a publisher they trust, as the README says. If that is not
good enough later, signing can be added back as a new `schema` (the minisign-based version is in
git history, before the commit "Remove index signing").

## Document

```json
{
  "schema": 1,
  "serial": 7,
  "generated": "2026-10-02T00:00:00Z",
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

## Rules and reason codes

| Check | Rule | Code |
|---|---|---|
| size | index at most 8 MiB (8,388,608 bytes), checked first | `too_large` |
| JSON | strict rules from [manifest.md](manifest.md) (no duplicate keys, `NaN`, BOM, lone surrogates, nesting deeper than 32) | `not_json`, `duplicate_key` |
| top level | an object; exactly the fields above | `bad_type`, `unknown_field`, `missing_field` |
| `schema` | integer 1 | `bad_schema` |
| `serial` | integer, 0 to 9,007,199,254,740,991 (2^53 - 1) | `bad_type` |
| `generated` | exactly `YYYY-MM-DDTHH:MM:SSZ`, a real calendar date and time, UTC | `bad_time` |
| freshness | `serial` not lower than the cached serial (equal is accepted) | `rollback` |
| `plugins` | list, at most 1,000; each entry exactly `id`, `versions` (else `unknown_field`/`missing_field`); `id` follows the manifest id rule (device names refused) and is unique | `bad_type`, `too_many`, `bad_id`, `duplicate` |
| `versions` | non-empty list, at most 200 per plugin; `version` and `min_manager` are SemVer with the manifest's length and range limits; `(id, version)` unique | `bad_type`, `too_many`, `bad_version`, `duplicate` |
| `manifest` | passes [manifest.md](manifest.md); its `id` and `version` equal the entry's | `bad_manifest`, `mismatch` |
| `url` | ASCII, at most 2,048 characters, no whitespace, scheme `https`, a host, no userinfo (even an empty one) | `bad_url` |
| `sha256` | 64 lowercase hex characters | `bad_hash` |
| `size` | integer from 1 to 67,108,864 (64 MiB) | `bad_size`, `bad_type` |

The first failing check wins, in the order of this table. The manager refuses the whole index on any
failure and keeps using the previously cached one.

## Rollback

- The manager stores the highest `serial` it has accepted. A lower serial is refused so that a
  stale mirror or a replayed older index cannot hide an update or reinstate a vulnerable version.
  An equal serial is accepted so that re-fetching an unchanged index works.
- Publishers raise `serial` on every publication. There is no expiry date: an index stays valid
  until a newer one replaces it.

## Package verification (used by the manager, Phase 4)

1. Download only the `url` from the checked index entry.
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

## Test vectors

`docs/vectors/index.json` has the cached serial (5) and a list of `{name, expect, index}` cases.
Oversize and too-many-entries cases are covered by unit tests rather than committed vectors (they
would be megabytes). Regenerate with `python3 tools/gen_index_vectors.py`; run with
`bash tools/run_all.sh`.
