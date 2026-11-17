# ADR 003: Single WAL-Mode SQLite Database for Incremental Index Storage

- **Status:** Accepted
- **Date:** 2026-10-31
- **Task:** T12 (`Incremental index & perf`)
- **References:** `PLAN.md` §0 #4, §0 #5, §0 #8, §3.1–§3.3, §7, §9, §10 T12

## Context

`PLAN.md` §10 T12 requires an Architecture Decision Record evaluating **SQLite vs flat files** for Kakeibo's incremental derived-record cache in `kakeibo-index`, the sole crate permitted to write to disk inside Kakeibo's cache/config directories (`PLAN.md` §0 #5, §3.1).

The incremental index must support the full discovery-to-report pipeline (`discover -> index -> parse -> dedupe -> attribute -> flag -> price -> report`, `PLAN.md` §3.2) under four constraints:

1. **Warm-run latency budget (`PLAN.md` §7):** On a 1 GiB corpus spanning hundreds or thousands of session files across six coding agents, a warm run with zero changed files must complete in **`<= 300 ms`** with peak RSS **`<= 300 MB`**.
2. **Exact append resume & truncation safety (`PLAN.md` §7, §10 T12):** When an active JSONL or Markdown session log grows by `M` bytes on the same `(dev, ino)` inode, Kakeibo must seek directly to `committed_offset` (the byte offset immediately after the last `\n`), read only the `M` newly appended bytes, resume from a text-free parser checkpoint, and produce output **identical** to `--no-cache` (`cached == uncached` property). Truncations, rotations (`(dev, ino)` change), in-place rewrites (`mtime` change at equal size), or SQLite `-wal` updates (`opencode.db-wal`, `state.vscdb-wal`) must invalidate stale state automatically.
3. **Strict privacy & pathlessness (`PLAN.md` §0 #4, §9):** Neither raw transcript text nor raw user filesystem paths may be persisted in the cache. Planted marker strings and raw working-directory paths must appear **zero times** in the on-disk index.
4. **Concurrency & self-healing:** Multiple `kakeibo` invocations (for example, an interactive `kakeibo tui`, a CLI `kakeibo autopsy`, and a background `kakeibo snapshot` refresh for the macOS menu bar app) may read and update `<cache_dir>` concurrently without corrupting cached state or failing user commands.

## Decision

We select a **single WAL-mode SQLite database** at `<cache_dir>/index.sqlite` implemented in `kakeibo-index::store` using the workspace's existing bundled `rusqlite` dependency (`0.40`, `features = ["bundled"]`), adding **zero** new storage crates:

1. **Connection pragmas & concurrency:**
   - `PRAGMA journal_mode = WAL;`
   - `PRAGMA synchronous = NORMAL;`
   - `PRAGMA busy_timeout = 5000;`
   - `PRAGMA foreign_keys = ON;`
   WAL mode allows concurrent readers alongside a single writer, while `busy_timeout = 5000` serializes concurrent cache updates cleanly without `SQLITE_BUSY` failures.

2. **Pathless, text-free schema (`index_schema_version = 1`):**
   - `index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)` records `index_schema_version` and `binary_version` (`kakeibo_core::product::VERSION`). Any schema/version mismatch or `SQLITE_CORRUPT` / `SQLITE_NOTADB` error drops and recreates `<cache_dir>/index.sqlite` transparently inside `<cache_dir>`.
   - `indexed_sources` is keyed by an irreversible SHA-256 `source_id`:
     $$\text{source\_id} = \text{SHA-256}(\text{domain\_tag} \parallel \text{agent} \parallel \text{source\_kind} \parallel \text{parent\_session\_id} \parallel \text{canonical\_path\_bytes})$$
     Each row stores:
     - `stat` fingerprint: `(dev, ino, last_seen_size, mtime_sec, mtime_nsec)` plus companion WAL `stat` `(aux_size, aux_mtime_sec, aux_mtime_nsec)` so uncommitted WAL writes on `opencode.db-wal` and Cursor `state.vscdb-wal` invalidate cached rows before SQLite checkpoints the main DB file.
     - `committed_offset`: byte offset immediately after the last `\n` consumed into `checkpoint_json`.
     - `checkpoint_json`: versioned JSON encoding of the parser's **text-free** stream state at `committed_offset` (`SessionState` / `PendingTurn` byte counts, `BlockFingerprint` hashes, `BTreeSet` dedupe IDs, cumulative token baselines, and physical `line_number`).
     - `sessions_json` and `diagnostics_json`: deduplicated `Vec<TurnRecord>` batches and `CachedDiagnostics` reflecting the source up to `last_seen_size` (including any non-`\n`-terminated trailing EOF line evaluated on a clone of the `\n`-committed checkpoint).

3. **Single-transaction batch update & stale-row pruning:**
   - Warm lookup loads all `indexed_sources` metadata headers in one sequential scan, classifies each discovered source in memory (`Unchanged`, `ResumeFromOffset`, or `FullReparse`) after a single `fs::metadata` (`stat`) syscall per discovered file, decodes only cold/appended files in parallel via `rayon`, and commits all updated rows plus deleted-source pruning in **one** `IMMEDIATE` SQLite transaction.

4. **`--no-cache` isolation (`PLAN.md` §3.2):**
   - When `--no-cache` is passed, `kakeibo` runs in `IndexMode::Uncached` and neither opens, reads, creates, nor modifies `<cache_dir>/index.sqlite`.

## Alternatives Considered

### Flat per-source JSON / NDJSON shard files (`<cache_dir>/sources/<sha256>.json`)

- **Pros:**
  - Simple human-inspectable files using `serde_json` alone; atomic single-file replacement via temporary file + `rename(2)`.
- **Cons:**
  - **Filesystem syscall bottleneck on warm runs:** Opening, reading, and closing 1,000–5,000 small cache shard files requires thousands of `open` + `read` + `close` syscalls on macOS APFS (where per-file `open` overhead is relatively high), eating a large fraction of the `<= 300 ms` warm budget (`PLAN.md` §7) before attribution even starts.
  - **No multi-file transactionality or atomic stale pruning:** Pruning deleted logs or upgrading the cache schema across thousands of shard files cannot execute atomically, risking half-migrated directories when two `kakeibo` processes run concurrently.
  - **No binary-size savings:** `rusqlite` (`bundled`) is already linked into `kakeibo` to read OpenCode (`opencode.db`) and Cursor (`state.vscdb`) databases (`PLAN.md` §3.3, §5), so avoiding SQLite in `kakeibo-index` saves zero bytes in the compiled binary.

### Single monolithic JSON / bincode snapshot file (`<cache_dir>/index.json`)

- **Pros:**
  - Single sequential `open` + `read` on warm startup.
- **Cons:**
  - Appending 200 bytes to a single active Claude Code transcript forces re-serializing and atomically rewriting the entire multi-megabyte corpus cache on every invocation (`O(total_corpus)` write amplification instead of `O(changed_sources)`).
  - Concurrent writers (`tui` + `snapshot`) suffer lost-update races without advisory file locking across platforms.

## Consequences

- `kakeibo-index` reuses the workspace's existing bundled `rusqlite` dependency while preserving the strict crate graph (`kakeibo-index -> kakeibo-core` only; `DependencyGraphTests` remains enforced).
- Warm runs issue one SQLite query plus lightweight `stat` checks across discovered log paths, reading **0 bytes** from unchanged session logs and **only `[committed_offset .. new_size]`** from appended line-oriented logs.
- Raw transcript contents and raw filesystem paths never touch `<cache_dir>/index.sqlite`; opt-in `--show-paths` continues extracting session `cwd` strings directly into transient memory only when explicitly requested by the user.
