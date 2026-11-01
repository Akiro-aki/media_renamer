# Original v1 plan (2026-10-31)

> Archived copy of the plan approved at the start of the project. Kept for reference; current status is in DEVELOPMENT.md.


## Context
FileBot is closed source and paid. The user wants a Linux alternative that matches messy media filenames against online databases and renames/moves them into clean library layouts (Plex/Jellyfin/Kodi/Komga/Kavita). `/mnt/shared/dev/Projects/media_renamer` is empty, so this is a greenfield build.

Decisions made with the user:
- **Rust**, **CLI first**. The core is a separate library crate so a GUI can be added on top later.
- Media: **TV, movies, anime, comics, manga**.
- Extras: **undo/history** (always on) and **artwork + .nfo/ComicInfo.xml** (opt-in, off by default).

Toolchain present: rustc/cargo 1.99, ffprobe (not needed for v1).

## Layout (cargo workspace)
```
media_renamer/
  Cargo.toml                 # workspace
  crates/mrn-core/src/
    lib.rs
    scan.rs        # walk inputs, classify by extension, attach sidecars (.srt/.ass/.nfo/.jpg with same stem)
    parse/         # filename → ParsedInfo (hand-written, regex based)
      mod.rs, video.rs, book.rs, tokens.rs
    detect.rs      # decide MediaKind: Movie | Tv | Anime | Comic | Manga
    providers/
      mod.rs       # Provider trait, Candidate, Metadata, Episode/Volume types
      tmdb.rs      # movies + TV (+ anime episode titles)
      anilist.rs   # anime + manga (GraphQL, no key)
      mangadex.rs  # manga fallback (no key)
      comicvine.rs # western comics (free key)
      http.rs      # shared reqwest client, per-host rate limiter, retry with backoff
    cache.rs       # SQLite response cache with TTL (~/.cache/mrn/cache.db)
    matcher.rs     # rank candidates: jaro-winkler title + year/kind bonuses → confidence
    format/        # template engine + presets
      mod.rs, template.rs, presets.rs, sanitize.rs
    ops.rs         # plan → execute: move/copy/hardlink/symlink, conflicts, cross-fs move fallback
    history.rs     # SQLite batch log (~/.local/share/mrn/history.db), undo
    extras/        # opt-in
      nfo.rs, artwork.rs, comicinfo.rs
    config.rs      # ~/.config/mrn/config.toml + env overrides (XDG via `directories`)
  crates/mrn-cli/src/main.rs  # clap derive; preview table, prompts, progress
  tests/fixtures/filenames.toml  # parser corpus: input → expected fields
```

## Core design

**Parsing** (`parse/`): strip `[Group]`, `[CRC32]` and `(1080p)` tags, normalize `._` to spaces, then pull out tokens:
- **Video:** `S01E02E03`, `1x02`, air dates `2024.01.02`, anime absolute numbers (`- 12`, `E12`, ` 12v2`), year, resolution, source, codec, and release group.
- **Books:** `v01`/`Vol. 1`, `c001`/`Ch. 12`, `#012`, issue year, and `Annual`.

If a field is missing, fall back to the parent folder names (e.g. `Show/Season 2/03.mkv`). The parser is test-driven against a corpus of roughly 150 real-world names.

**Detection** (`detect.rs`):
- `.cbz/.cbr/.cb7/.pdf/.epub` are books. A file is manga if it has `Ch./c###/Vol.` tokens, a comic if it has `#NNN` or an issue year.
- Videos with `SxxEyy` or a date are TV. Videos with a bracketed group plus an absolute number are anime. Videos with a year and no episode are movies.
- `--type` always overrides detection.

**Providers** (`Provider` trait: `search`, `details`, `episodes` / `volumes`):
| Kind | Primary | Notes |
|---|---|---|
| Movie, TV | TMDB v3 | Needs a free API key (`TMDB_API_KEY` or config) |
| Anime | AniList | Gives the series identity. Episode titles come from TMDB, with seasons flattened into absolute order (specials excluded). Titles are left out if the mapping fails |
| Manga | AniList → MangaDex fallback | No keys needed |
| Comics | ComicVine | Free key. Without one, comics get parse-only renames |

Every request goes through `http.rs`, which applies per-host rate limits (AniList ~30/min, ComicVine 200/hr), retries 429s, and uses the SQLite cache.

**Matching**: confidence comes from normalized title similarity (`strsim` jaro-winkler, alt titles/romaji included), plus a year match, plus agreement on media kind. Matches at or above the threshold (default 0.85) are accepted. Below it, the CLI shows the top 5 to choose from, or skips the file with `--non-interactive`. `--query "Title"` and `--id tmdb:1234` / `anilist:5114` force a match.

**Format templates** (our own small language, not Groovy):
- `{n}` name, `{y}` year, `{s}` / `{e}` with padding (`{s:02}`), `{s00e00}`, `{t}` episode title, `{abs:03}`, `{vol:02}`, `{ch:03}`, `{issue:03}`, `{vf}` resolution, `{group}`, `{ext}`.
- `[ ... ]` marks an optional section. It is dropped if any field inside it is empty.
- Presets: `plex`, `jellyfin`, `kodi`, `anime`, `komga`, `kavita`. For example, TV: `{n} ({y})/Season {s:02}/{n} - {s00e00}[ - {t}]`; manga: `{n}/{n}[ Vol.{vol:02}][ Ch.{ch:03}]`; comics: `{n} ({y})/{n} #{issue:03} ({y})`.
- `sanitize.rs` replaces `/` and NUL, maps `:` to ` - `, trims trailing dots and spaces, and caps each path component at 255 bytes.

**Ops** (`ops.rs`):
- Builds a full plan first: `Vec<Op{from, to, sidecars}>`. Sidecars move with their main file.
- Actions: `move` (default), `copy`, `hardlink`, `symlink`. A move across filesystems falls back to copy, then fsync, then remove.
- Conflicts: `skip` (default), `override`, or `index` (` (1)`). Two plan entries that target the same path are caught before anything runs.
- Empty source dirs are pruned with `--clean`.

**History**: every executed batch is written to SQLite before and after each op (from, to, action, timestamp). `mrn undo [BATCH]` reverses a batch in reverse order and refuses to overwrite files that changed since. `mrn history` lists batches.

**Extras** (opt-in through `--nfo`, `--artwork`, or `[extras]` in config; off by default):
- `nfo.rs` writes Kodi/Jellyfin `movie.nfo`, `tvshow.nfo`, and per-episode `.nfo` files using `quick-xml`.
- `artwork.rs` downloads `poster.jpg`, `fanart.jpg`, and `seasonNN-poster.jpg` from TMDB/AniList image URLs. It skips files that already exist.
- `comicinfo.rs` builds ComicInfo.xml. For `.cbz` it is injected into the archive by rewriting it through a temp file with the `zip` crate. `.cbr/.pdf` get a sidecar or are skipped, since RAR can't be written.

## CLI (`mrn-cli`)
```
mrn rename <PATHS>... [--type auto|tv|movie|anime|comic|manga] [--format PRESET|TEMPLATE]
           [-o, --output DIR] [--action move|copy|hardlink|symlink] [--conflict skip|override|index]
           [--query TITLE] [--id SRC:ID] [--lang en] [--nfo] [--artwork] [--clean]
           [-n, --dry-run] [-y, --yes] [--non-interactive]
mrn info <FILE>        # show parse result, detected kind, top candidates
mrn history | mrn undo [BATCH]
mrn config init|show|path
```
Flow: scan → parse → detect → match (async with tokio, bounded concurrency) → show a preview table (`comfy-table`, original → new) → confirm (`dialoguer`) → execute with progress (`indicatif`) → record history.

Config: `~/.config/mrn/config.toml` holds API keys, a default format per kind, an output root per kind (e.g. `tv = "~/Media/TV"`), the default action, the language, and `[extras]`.

## Key dependencies
`clap`, `serde`/`toml`, `tokio`, `reqwest` (rustls, json), `regex`, `walkdir`, `strsim`, `unicode-normalization`, `rusqlite` (bundled), `directories`, `dialoguer`, `indicatif`, `comfy-table`, `quick-xml`, `zip`, `anyhow`/`thiserror`, `tracing`. Dev: `tempfile`, `wiremock`, `insta`.

## Build order
1. Set up the workspace (`git init`, workspace Cargo.toml, both crates) plus `config.rs`.
2. `parse/` + `detect.rs` + the fixture corpus. Have `mrn info` working offline.
3. `format/` + `ops.rs` + `history.rs`. `mrn rename --id/--query` with dry-run, execute, and undo working end to end.
4. TMDB provider + cache + rate limiter + matcher. Movies and TV fully working.
5. AniList (anime and manga), MangaDex, ComicVine.
6. Extras: nfo, artwork, ComicInfo.xml.
7. Polish: interactive candidate picker, `--non-interactive`, README with config examples.

## Verification
- `cargo test --workspace`:
  - parser corpus tests (every fixture's expected fields)
  - template/sanitize unit tests
  - `wiremock` tests for each provider using recorded JSON
  - ops/history integration tests in a `tempfile` dir: create dummy files → `rename` → assert the layout → `undo` → assert the originals are restored, including the cross-fs fallback and conflict modes
- `cargo clippy --workspace -- -D warnings`.
- Manual end to end with a real TMDB key: `truncate -s 1M` dummy files named like real releases (`The.Expanse.S02E05.1080p.WEB.mkv`, `[SubsPlease] Frieren - 12 (1080p) [ABCD1234].mkv`, `Dune.2021.2160p.mkv`, `One Piece v01.cbz`, `Saga #001 (2012).cbz`) → `mrn rename -n` preview → real run → `mrn history` → `mrn undo`.
- Run once with `--nfo --artwork` to check the sidecar files and that ComicInfo.xml ends up inside the cbz (`unzip -l`).
