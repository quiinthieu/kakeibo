# ADR 001: Use `time` for Pure RFC 3339 UTC Timestamps

- **Status:** Accepted
- **Date:** 2026-10-31
- **Task:** T03 (`Core model & ledger`)
- **References:** `PLAN.md` §0 #3, §0 #8, §3.3, §4

## Context

`PLAN.md` §3.3 permits either `jiff` or `time` (requiring an ADR) to represent `Timestamp` on `kakeibo_core::TurnRecord`.

Agent session logs emit RFC 3339 / ISO 8601 timestamps (for example `"2026-10-31T09:00:04Z"`, `"2026-10-31T09:00:04.125Z"`, or fixed-offset forms such as `"2026-10-31T16:00:04+07:00"`), as well as integer Unix epoch seconds/milliseconds in SQLite-backed agents (opencode, Cursor).

At the same time, `PLAN.md` §0 #3 imposes a strict architectural rule on `kakeibo-core`:

> `kakeibo-core` is pure: no I/O, clock or env.

We need a timestamp representation in `kakeibo-core` that:

1. Parses and formats RFC 3339 timestamps with subsecond precision and deterministic UTC normalisation.
2. Converts cleanly to and from Unix epoch seconds/nanoseconds for cache-TTL idle-gap arithmetic (`cold-resume` flag in T07).
3. Performs **zero** wall-clock queries, timezone database lookups, or environment variable reads (`TZ`).
4. Keeps compile times, transitive dependency count, MSRV (`1.88`), and `cargo deny` license compliance tight.

## Decision

We select **`time` `0.3`** configured with minimal features:

```toml
time = { version = "0.3", default-features = false, features = ["formatting", "parsing", "serde-well-known"] }
```

And wrap `time::OffsetDateTime` privately inside `kakeibo_core::Timestamp`:

1. **No local-offset or macro features:** `local-offset` and `macros` are disabled. No `TZ` environment reads, OS timezone syscalls, or proc-macro crates are pulled into the build graph.
2. **Canonical UTC normalisation:** Every parsed RFC 3339 timestamp (`OffsetDateTime::parse(..., &Rfc3339)`) and Unix epoch timestamp is immediately converted to `UtcOffset::UTC` inside `Timestamp`. Two timestamps representing the same instant (e.g. `2026-10-31T16:00:04+07:00` and `2026-10-31T09:00:04Z`) compare equal (`Eq`, `Ord`, `Hash`) and serialise identically as canonical UTC RFC 3339 strings ending in `Z`.
3. **No clock constructors exposed or permitted:** `kakeibo_core::Timestamp` exposes only pure constructors (`parse_rfc3339`, `from_unix_timestamp`, `from_unix_timestamp_nanos`). `DependencyGraphTests` additionally bans `SystemTime`, `Instant::now`, `now_utc`, and `now_local` across `crates/kakeibo-core/src/**/*.rs`.

## Alternatives Considered

### `jiff`

- **Pros:** Modern API, rich civil/zoned datetime types, built-in IANA `tzdb` handling.
- **Cons:** Kakeibo only needs instant-level UTC timestamps and elapsed duration math (`ts_b - ts_a`), never civil calendar arithmetic or named IANA timezones. Carrying bundled `tzdb` tables and local-zone machinery adds unnecessary binary weight and surface area for an instant-only ledger field.

### Hand-rolled RFC 3339 parser

- **Pros:** Zero external crates.
- **Cons:** Error-prone around leap years, leap seconds (`:60`), variable fractional-second digits (1–9 digits), and signed offset bounds (`±23:59`), whereas `time`'s RFC 3339 parser is battle-tested and permissively licensed (`MIT OR Apache-2.0`).

## Consequences

- `kakeibo-core` gains a lightweight, deterministic `Timestamp` value type (`Copy + Eq + Ord + Hash + Serialize + Deserialize + Display + FromStr`).
- Round-tripping any `TurnRecord` through `kakeibo.ledger/1` NDJSON preserves exact nanosecond instants and emits canonical `...Z` strings.
- Purity invariants remain mechanically enforced by `crates/kakeibo/tests/dependency_graph.rs` and `cargo deny check`.
