# Architecture

How `mrn` is put together and why. For project status, known issues and the
roadmap, see [DEVELOPMENT.md](DEVELOPMENT.md). For user docs, see the [README](../README.md).

## Overview

```
 ┌──────── mrn-cli (bin "mrn") ────────┐   ┌──────── mrn-gui (bin "mrn-gui", iced) ───────┐
 │ main.rs   clap args, dispatch       │   │ main.rs        app shell, tabs, file drop    │
 │ rename.rs rename + info, confirm    │   │ worker.rs      job thread, GuiChooser        │
 │ ui.rs     PromptChooser, spinner    │   │ rename_tab.rs  sources, options, preview     │
 │ manage.rs history/undo/config/cache │   │ history_tab.rs / settings_tab.rs / widgets.rs│
 └──────────────────┬──────────────────┘   └───────────────────────┬──────────────────────┘
                    └──────── pipeline::Engine + preview::Preview ──┘
┌──────────────────────────── mrn-core (lib) ──────────────────────────────┐
│ scan ─► parse ─► detect ─► providers + matcher ─► format ─► ops ─► history│
│                                   │                          └─► extras  │
│                         cache (SQLite) / http (rate limit, retry)        │
│ pipeline.rs: Engine::analyze → plan → execute (glues everything)         │
└──────────────────────────────────────────────────────────────────────────┘
```

Both front ends are thin. They call the same `Engine` methods and show the
same `preview::Preview`; they differ only in their `Chooser`.

### The GUI (`mrn-gui`)

- iced 0.14 (Elm-style: `update`/`view`), system light/dark theme, file pickers through the
  XDG desktop portal (`rfd` with `xdg-portal`, so no GTK is linked).
- A rename runs in `worker::start` on its own thread with its own tokio runtime, because
  `Chooser::choose` blocks until the user answers. The worker sends `Event`s (status, `Choose`
  prompt, `Preview`, progress, `Finished`) over a futures channel that the GUI consumes with
  `Task::run`; the GUI answers with `Command::Choice`/`Command::Apply` over a std channel.
- The worker parks after the preview until `Apply`. Dropping the `Job` cancels: pending prompts
  answer "skip" and nothing is applied. Changing any option drops the job (the preview is stale).
  Events carry a job id so a cancelled job's late events are ignored.
- History, undo and config writes run on short-lived threads (`worker::blocking`).
- Settings edits only `[api]` through `config::save_api_keys` (toml_edit, keeps comments, 0600).
- Drag and drop works on X11 only: winit has no Wayland DnD yet.

## Source map

| File | Responsibility |
|------|----------------|
| `mrn-core/src/lib.rs` | `MediaKind` (movie, tv, anime, comic, manga) |
| `scan.rs` | Walk inputs, keep video/book extensions, skip hidden files and small `sample` videos, attach sidecars |
| `parse/mod.rs` | `ParsedInfo`, `parse_path`, extension lists |
| `parse/tokens.rs` | Shared regexes, tech tokens (resolution/source/codec), separator normalization, `season_folder` |
| `parse/video.rs` | Movie/TV/anime filename parser plus parent-folder context |
| `parse/book.rs` | Comic/manga filename parser plus parent-folder context |
| `detect.rs` | `detect(&ParsedInfo) -> MediaKind`, `resolve_book_numbers` |
| `matcher.rs` | Title normalization, similarity, candidate scoring and ranking |
| `providers/mod.rs` | `Provider` trait, `Candidate`, `Episode`, `SeriesInfo`, `Issue`, `Source`, `ForcedId`, `Providers` registry |
| `providers/http.rs` | Shared reqwest client: per-host token bucket, retries, cache, secret stripping |
| `providers/{tmdb,anilist,mangadex,comicvine}.rs` | One API each; all take `with_base_url` for tests |
| `cache.rs` | SQLite response cache with TTL |
| `format/template.rs` | Template language: parser and renderer |
| `format/presets.rs` | Built-in presets per kind (fallback lists) |
| `format/sanitize.rs` | Path-safe values and components |
| `format/mod.rs` | `Fields`, `Value`, `Formatter` (template list → relative path) |
| `ops.rs` | `Action`, `Conflict`, `build_plan` (conflicts), `apply` (fs ops), `move_file`, `copy_file`, `prune_empty_dirs` |
| `history.rs` | SQLite batch/op log and `undo` |
| `trash.rs` | freedesktop.org trash on the file's own file system: `Trash::put`, `restore` |
| `extras/mod.rs` | Which extra files to create where (`plan`) |
| `extras/nfo.rs` | Kodi/Jellyfin nfo XML and a tiny XML builder/escaper |
| `extras/comicinfo.rs` | ComicInfo.xml build, `.cbz` injection (`inject`) and its reversal (`restore`); `rewrite` changes one archive entry |
| `extras/cover.rs` | Kavita's cover-image rule (`is_cover_image`), embedding a volume cover (`embed`) and removing it (`remove`) |
| `config.rs` + `config_template.toml` | Config struct, load and env overrides, XDG dirs, starter template |
| `pipeline.rs` | `Options`, `Chooser`, `Engine`, `Item`, `Analysis`, `ExecEvent`, `build_fields` |
| `preview.rs` | `Preview`/`PreviewRow`/`RowState`: display-ready rows shared by CLI and GUI; `relative`, `ago` |

## Runtime files (XDG, via the `directories` crate)

| What | Path | Override |
|------|------|----------|
| Config | `~/.config/mrn/config.toml` | `--config`, `$MRN_CONFIG` |
| Cache | `~/.cache/mrn/cache.db` | `$XDG_CACHE_HOME` |
| History | `~/.local/share/mrn/history.db` | `$XDG_DATA_HOME` |
| Replaced files (`--conflict override`) | `~/.local/share/Trash`, or `<mount>/.Trash-$UID` for other file systems | `$XDG_DATA_HOME` |
| API keys | config `[api]` | `$TMDB_API_KEY`, `$COMICVINE_API_KEY` (env wins) |
| Logging | stderr | `-v`/`-vv`, `$RUST_LOG` |

## The pipeline (`pipeline.rs`)

`Engine::analyze(files, &Options, &mut dyn Chooser, status)` runs in eight steps:

1. **Parse and detect.** `parse_path` gives a `ParsedInfo`. The kind is `--type` or `detect()`. `resolve_book_numbers` turns a bare book number into `chapter` (manga) or `issue` (comic).
2. **Group.** Files are grouped by `(kind, normalize(query), year)`, where the query is `--query` or the parsed title. Each group is looked up once. Files with no title fail with "could not read a title".
3. **Search** (concurrent, `general.concurrency`). Each group calls `search_group`:
   - `--offline` → `Local`.
   - `--id` → `lookup` → `Matched` (score 1.0).
   - No provider for the kind: comics → warn + `Local`; movie/TV → `Failed` with the missing-key message.
   - Otherwise search the providers in priority order. The first candidate scoring ≥ `threshold` is accepted. If none is, the merged top 10 go to step 4.
4. **Resolve uncertain groups** one at a time.
   - Non-interactive → `Failed("uncertain match … use --query or --id")`.
   - Interactive → `Chooser::choose` returns `Pick(i)`, `Skip`, or `Search(q)`. `Search` re-searches with the group's year and asks again.
5. **Episode lists** (concurrent) for matched TV/anime groups, via `series_for`:
   - TMDB match → `tmdb.series()`.
   - AniList anime → cross-reference TMDB: search TV by the English title, then the configured title. Accept a result scoring ≥ `CROSS_REF_THRESHOLD` (0.85), then fetch its series.
   - No TMDB key → nothing (anime keeps absolute numbering, no titles).
6. **Formatters.** One `Formatter` per kind present: `--format` (preset name or template), then the config `[format].<kind>`, then the config preset. A bad template fails the whole run early.
7. **Comic issue titles** (concurrent), only if `--comicinfo` or covers are on, or the comic template uses `{t}`. ComicVine allows only 200 requests per hour.
8. **Series details** (concurrent), only with `--comicinfo`: `Provider::book_details` once per matched book series (AniList, MangaDex, MangaBaka; empty for ComicVine), stored in `Match.book`.
9. **Volume covers** (concurrent), only with `--embed-cover` or `--artwork -f komga`: `Engine::manga_covers` once per matched manga series, stored in `Match.covers`. The MangaDex id is the match's own, else `ext_ids`, else a MangaDex title search (`mangadex_by_title`): an entry sharing the AniList or MAL id wins, one with a different id is dropped, otherwise the best title must score ≥ `CROSS_REF_THRESHOLD`. Then `Mangadex::volume_covers` (see below).
10. **Name each file.** `find_episodes` matches by season+episode, then absolute, then air date. `build_fields` fills the fields, and `render_target` produces the full target path or an error string.

`Engine::plan` turns items with targets into `ops::Request`s and calls `build_plan`
(which may reorder them, see below).

`Engine::execute` begins a history batch and then, for each runnable plan item:
- Run the main op (an `Overwrite` item passes `history.trash()` to `apply`), then `finish_op`.
- Embed ComicInfo as a separate `OpKind::Embed` op that stores the entry it replaced.
- Embed the book's cover (`--embed-cover`) as an `OpKind::Cover` op that stores the entry name it added. Images are downloaded once per URL per batch.
- Run the sidecar ops.
- Write extras, each recorded as an `OpKind::Create` op.
- With `--clean`, prune empty source dirs up to the scan root.

### Item fields (`build_fields`)

| Field | Comes from |
|---|---|
| `n` | Candidate title, or the parsed title |
| `y` | Candidate year, or the parsed year |
| `id`, `source`, `tmdbid` / `anilistid`, `publisher` | The match |
| `s`, `e`, `s00e00`, `sxe` | The filename's season/episodes if present; otherwise the provider episode mapped from absolute numbers |
| `abs` | Parsed absolute numbers, or the provider's absolute numbers |
| `t` | Unique episode titles joined with " & ", or the issue title |
| `d` | Parsed date, or the first episode's air date |
| `vol`, `ch`, `issue`, `vf`, `vs`, `vc`, `ac`, `group`, `crc`, `ot` | Parsed values |
| `orig` | Original file stem |

### Where files go

- **No library root** (no `-o` and no `[output].<kind>`): renamed in place. Only the last template component (the file name) is used, and the file stays in its folder.
- **With a root:** the full template path goes under the root. `Item.organized = true`.
- The extension is always the source's original extension.

## Parser (`parse/`)

Video parsing (`video.rs::parse`):

1. A leading `[Group]` becomes `group`, with `bracket_group = true`, unless it holds tech tokens (e.g. `[1080p]`).
2. Bracket groups `[...]`, `(...)`, `{...}` are classified and replaced with a `BOUNDARY` (`\u{1}`), which ends the title:
   - 8 hex chars → `crc32`
   - `(2021)` → `year`
   - `[01]` → bracket episode
   - tech content → fields
   - other square brackets → group candidate (last one with a letter wins, e.g. `[YTS.MX]`)
   - other parens such as `(US)` are kept as title text
3. `normalize_separators`: `_` and `.` become spaces, except a dot between digits (`5.1`, `2024.01.02`).
4. Markers set `title_end` (earliest wins), in this priority:
   - `SxxEyy` with lists/ranges (`E01E02`, `E01-E03`, `E01-03`)
   - `NxNN`
   - date `YYYY.MM.DD`
   - season-only (`S2`, `Season 3`)
   - dash number ` - 12` (not a year)
   - `Episode 5` / `Ep5`
   - `E12`
   - bracket episode
5. **Year:** the last `19xx`/`20xx` that isn't the first word and isn't inside a date. So `1917.2019.mkv` gives title 1917, year 2019, and `2012.mkv` has no year.
6. **Tech tokens:** the first *terminating* hit ends the title. Ambiguous `web`, `bd` and `opus` only fill fields, never end titles ("Charlotte's Web").
7. **Scene group:** `…x264-GROUP`, but not when a tech token spans the dash (`WEB-DL`).
8. **Fansub fallback:** `[Grp] Show 03 [1080p]` → absolute 3.
9. A season plus a dash number (`Show S2 - 05`) makes the number a season episode.

Folder context (`apply_folder_context`):
- Season folders (`Season 2`, `S02`, `Specials` = 0) supply the season.
- The first non-season ancestor supplies the title when the file has none, the title is only a number (`03.mkv`), the title starts with an episode number inside a season folder (`03 - Pilot.mkv`), or the name is generic (`movie`, `film`, `video`, `main`, `feature`, `episode`).
- The year is copied from the folder when the titles match.

Book parsing (`book.rs::parse`):
- Leading `[Group]`.
- Brackets: keep only the year; `(Digital)`, `(of 12)` and similar become boundaries.
- Markers: `v01`/`Vol. 1`/`Volume`, `c001`/`Ch. 12`/`Chapter`, `#012`.
- A year after the first word ends the title.
- A bare number (not the first word, not a year) goes to `number`.
- "Annual" stays in the title (ComicVine names those volumes "Batman Annual") and sets `annual`.
- Folder context gives a title for `Akira/001.cbz` and the year for `Saga (2012)/Saga 054.cbz`.

Detection (`detect.rs`):
- Book extensions:
  - `#`/annual → comic
  - chapter → manga
  - volume → manga, unless there is also a number and a year (`Batman v2 001 (2016)` → comic)
  - number → comic with a year, manga without
  - year only → comic
  - otherwise → manga
- Video:
  - fansub (bracket group or CRC) with episode numbers → anime
  - season/episodes/date → TV
  - absolute numbers → anime
  - otherwise → movie

The corpus `mrn-core/tests/fixtures/filenames.toml` (76 cases) pins this
behaviour. The test reports every mismatch at once.

## Matching (`matcher.rs`)

- `normalize`: NFKD, drop combining marks, lowercase, drop apostrophes, `&` → `and`, punctuation → space, collapse spaces, drop a leading `the`/`a`/`an`.
- `title_similarity`: equal → 1.0. Equal with spaces removed → 0.98 ("Spiderman"/"Spider-Man"). Otherwise `(jaro_winkler + normalized_levenshtein) / 2`. Jaro-Winkler alone scored "Dune" vs "Dune: Part Two" ≥ 0.85.
- `score`: the best similarity over the candidate's title and alt titles, against both the query and the query without parentheses ("The Office (US)" → "The Office").
  - Year adjustment: same year +0.08, ±1 year +0.03, further apart −0.2.
  - Rank adjustment: −0.005 × provider rank.
  - Clamped to 0..1.
- `DEFAULT_THRESHOLD` = 0.85 (`general.threshold`, `--threshold`).

## Providers

| Kind | Order | Notes |
|------|-------|-------|
| movie, tv | TMDB | v3 key → `api_key` query param; v4 token (`eyJ…`) → Bearer header. Retries the search without the year when the year gives no results |
| anime | AniList → MAL (`[providers] anime`) | AniList: GraphQL, `sort: SEARCH_MATCH`, title by `anime_title` preference (romaji default). MAL through Jikan, no key |
| manga | AniList → MangaBaka → MangaDex (`[providers] manga`) | Each later source only if the earlier ones have no confident match. MangaBaka: `/series/search?type_not=novel`, title from the `<lang>-Latn` primary title for romaji, AniList and MAL ids from `source`, merged series followed to `merged_with`; it has no MangaDex ids |
| comic | ComicVine | volumes via `/search/`, issues via `/issues/?filter=volume:ID,issue_number:N` |

TMDB `series()` fetches `/tv/{id}` and every season concurrently. It numbers
episodes of seasons ≥ 1 in order as `absolute`; specials have none.

`http.rs`:
- **Token bucket per host:** AniList 5 burst / 0.5 per second, MangaDex 5 / 4, Jikan 3 / 1, MangaBaka 5 / 0.5, ComicVine 10 / (200 per hour), TMDB 20 / 20, default 10 / 10.
- **Retries:** up to 4 attempts on 429/5xx (honouring `Retry-After`, capped at 60 s), timeouts and connect errors, with 1/2/4 s backoff between attempts (none after the last).
- **Unreachable hosts:** if all 4 attempts fail with timeouts or connect errors, that `Http` skips the host for 5 minutes and later requests to it fail at once, so the pipeline falls through to the next provider without waiting. A 429/5xx never triggers this.
- **Cache:** the key is the method + URL + query without secret params (`api_key`) + the POST body. TTL is `cache_ttl_hours` (72). `--refresh` skips cache reads.
- **Errors:** strip URLs with `without_url()` so API keys never reach messages.

## Templates (`format/`)

- **Syntax:** `{field}`, `{field:02}`, `{field:upper|lower|first}`, `[optional]`, `\` escapes, `/` folder separator.
- **Unknown field names** are rejected at parse time (`KNOWN_FIELDS`).
- **Missing values:** a required field without a value is an error. Inside `[ … ]` it drops that section.
- **Fallbacks:** a `Formatter` holds a list of templates and the first that renders wins. This is how daily shows (no SxxEyy) and anime without a season mapping fall back.
- **Numbers:** `format_number(12.5, 3)` → `012.5`. A list of numbers joins with `-` (`E05-06`).
- **Sanitizing** (`sanitize.rs`):
  - `": "` → `" - "`; `:` and `/` → `-`; control characters removed.
  - With `windows_safe` (default on): `\` → `-`, `"` → `'`, `?*<>|` removed, trailing dots and spaces trimmed.
  - Length caps: file stems 230 bytes (leaving room for sidecar suffixes), folders 255 bytes.

Presets (`presets.rs`):

| Preset | Movie | TV | Anime | Comic | Manga |
|---|---|---|---|---|---|
| jellyfin (default) | `{n} ({y}) [tmdbid-X]/{n} ({y})` | `{n} ({y})/Season {s:02}/{n} - {s00e00} - {t}`, daily fallback | TV layout, then absolute `{n} ({y})/{n} - {abs:02}` | `{n} ({y})/{n} ({y}) #{issue:03}`, one-shot fallback | `{n}/{n} Vol.{vol:02} Ch.{ch:03}` |
| plex | `{tmdb-X}` instead | same as jellyfin | same | same | same |
| kodi | no id | `{n}/Season {s}/{n} {s00e00}` | Kodi TV, then absolute | same | same |
| anime | jellyfin | jellyfin | absolute first (`{n}/{n} - {abs:02}`) | same | same |
| kavita | jellyfin | jellyfin | jellyfin | same | `Vol.`/`Ch.` |
| komga | jellyfin | jellyfin | jellyfin | same | `v01 c001` |

The table leaves out optional brackets. See `presets.rs` for exact strings.

## Operations (`ops.rs`)

`build_plan` handles conflicts:
- A target equal to the source → `Skip("already named correctly")`.
- Clash detection covers targets claimed earlier in the batch and existing files. A file that is the same inode as the source (a case-only rename or a hard link) is not a clash.
- `skip` → Skip. `override` → `Overwrite` for disk clashes but Skip for batch clashes. `index` → `Name (1).ext` … `(99)`.
- **Chain renames** (only with `move`): a target that another item moves away (A→B while B→C) is not a disk clash.
  `build_plan` assumes every source is vacated, plans, drops the sources whose items turned out not to run, and
  plans again until the assumption holds. `run_order` then sorts the items (Kahn, stable) so the item that frees a
  path runs first. Items in a cycle (A→B, B→A) are skipped ("would trade names").
- Sidecar targets are derived from the final main target (`<new stem><suffix>`).

`apply`:
- Creates parent directories and returns the ones it created.
- Refuses an existing target unless given a `Trash`; then the existing file is moved to the trash first (put back if
  the operation fails) and returned in `Applied::replaced`. The comparison doesn't follow symlinks, so a link to the source counts as a different file.
- `move` = `rename`, with copy + fsync + remove on `CrossesDevices`.
- `copy` writes `.<name>.mrn-part`, keeps the mtime, fsyncs, then renames.
- `hardlink`; `symlink` (to the absolute source path).

## History and undo (`history.rs`)

Schema:

```sql
batches(id, created, action, args, status)          -- running | done | partial | undone
ops(id, batch, kind, src BLOB, dst BLOB, status,     -- pending | done | failed | undone
    size, mtime_ns, overwrote, error,
    backup BLOB,                                     -- trash path of the file this op replaced
    data BLOB)                                       -- embed: the previous ComicInfo.xml (NULL = none)
dirs(batch, path BLOB)                               -- folders the batch created
```

Paths are stored as raw bytes, so non-UTF-8 names work.

- **Recording:** `start_op` (pending) runs before touching the disk; `finish_op` stats the target afterwards
  and records the trash path of a replaced file; `finish_embed` records the replaced ComicInfo.xml.
- **Migration:** `History::open` adds `backup`/`data` to older databases. Old ops with `overwrote = 1` and no
  backup still warn that the replaced file can't be restored.
- **`undo` order:** reverse op order.
  - `move` → move back, refusing if the original path is occupied.
  - `copy` / `hardlink` / `symlink` / `create` → delete.
  - then, if the op replaced a file, move it back from the trash and remove its `.trashinfo`.
  - `embed` → rewrite the archive with the previous ComicInfo.xml (or none), keeping its mtime. `cover` →
    remove the entry it added. Both (`OpKind::edits_in_place`) are skipped when a copy in the same batch made
    the archive (it is deleted next), aren't counted as files in batch totals, and afterwards the earlier ops
    on that archive are re-stat'ed, since they recorded it before the edit.
  - Ops whose target changed (size or mtime) are skipped unless `force`.
  - Created dirs are then removed deepest-first if empty.
  - The batch becomes `undone`, or `partial` if anything was skipped. `mrn undo` picks the newest batch that isn't `undone`.

## Extras (`extras/`)

`extras::plan` decides paths. `Engine::write_extras` skips paths that exist or
were already written this batch, then writes text through a temp file or
downloads images.

| Kind | nfo | artwork |
|---|---|---|
| movie | `<stem>.nfo` | `<stem>-poster.jpg`, `<stem>-fanart.jpg` |
| tv/anime | `<stem>.nfo` (episodedetails); `tvshow.nfo` in the series dir* | `poster`, `fanart`, `seasonNN-poster` / `season-specials-poster` in the series dir* |
| comic/manga | (ComicInfo instead) | `cover.<ext>` in the series folder*; with `-f komga`, also the book's own cover as `<stem>.<ext>` beside it |

\* Only when `organized` (a library root was used). The series dir is the
target's folder, or its parent if that folder is a season folder.

**Book covers.** A book's cover is the comic issue's (`Issue.cover` from
ComicVine) or its manga volume's. `Mangadex::volume_covers` reads
`/cover?manga[]=ID&order[volume]=asc` (100 per page, at most 10 pages) and keeps
one per volume: the manga's `originalLanguage` locale, else `en`, else any. For
chapter files it also reads `/manga/{id}/aggregate` to find each chapter's
volume. Uploaders misfile chapters (Chainsaw Man lists chapter 232 in volume 1),
so a chapter more than 3 median absolute deviations (and more than the volume's
chapter count) from the rest of its volume is ignored there. A chapter in no
volume, or a volume without a cover, gets no cover. MangaDex only lists the
chapters it hosts, so licensed series often have few.

- Kavita takes a book's cover only from inside the archive, and would list a
  `<stem>.jpg` beside it as an extra image chapter (manga libraries scan
  images by default). `--embed-cover` adds `!cover.<ext>` as the first entry,
  unless an image already matches Kavita's `CoverImageRegex` (`cover::is_cover_image`,
  tested with Kavita's own cases). It is an extra page.
- Komga prefers a `<stem>.<ext>` beside the book over its first page, so with
  `--artwork -f komga` (the preset named by `--format`, else the config's) the
  cover is saved there and the archive is not touched.

ComicInfo is embedded only with `move` or `copy`, only into `.cbz`/`.zip`.
So are covers.
From the series details it adds `LocalizedSeries` (the English title when it
differs; a Kavita extension that Komga ignores), `Count` (total volumes, or
chapters for chapter files, only for finished series), `Writer`, `Penciller`,
`Tags` (at most 15, without genres or spoilers) and `AgeRating` (a label both
Kavita and Komga know). Kavita and Komga split lists on commas, so commas
inside a name or tag are dropped.

| Source | Tags | Creators | Count | AgeRating |
|---|---|---|---|---|
| AniList | rank ≥ 60, no spoilers, highest first | Story → Writer, Art → Penciller, Story & Art → both (notes in brackets ignored) | `volumes`/`chapters` when `FINISHED` | Adults Only 18+ when `isAdult` |
| MangaDex | `theme`/`format`/`content` tags (`genre` ones → Genre) | author → Writer, artist → Penciller | `lastVolume`/`lastChapter` when `completed` | from `contentRating` |
| MangaBaka | `tags_v2` with weight `core` | `authors` → Writer, `artists` → Penciller | `final_volume`/`total_chapters` when `completed` | from `content_rating` |

`contentRating`: safe → Everyone, suggestive → Teen, erotica → Mature 17+,
pornographic → Adults Only 18+.
Existing entries are copied raw (no recompression), any old `ComicInfo.xml` is
replaced (and returned for the history), the archive keeps its mtime and is
swapped in atomically.

## Trash (`trash.rs`)

Follows the freedesktop.org trash spec, but only ever uses a trash on the
file's own file system, so trashing is a rename: the home trash when the file
shares its device, else `$top/.Trash/$uid` (an admin `.Trash` with the sticky
bit) or `$top/.Trash-$uid` at the mount root. The `.trashinfo` is created with
`create_new` first to reserve the name; `Path=` is absolute for the home trash
and relative to the mount root otherwise. If no trash is usable, the overwrite
fails and the existing file stays.

## Tests

| Suite | What |
|---|---|
| unit tests (69, plus 1 ignored live AniList test) in `mrn-core/src/**` | template, sanitize, presets, matcher, ops (incl. chain renames, replace via trash), history (incl. trash restore, ComicInfo and cover revert, migration), trash, cache, http (wiremock, incl. unreachable-host skip), each provider (wiremock, several on trimmed real responses in `tests/fixtures/`), extras (incl. Komga sidecars), nfo, comicinfo, cover (Kavita's cover-name cases), scan, config template |
| `mrn-core/tests/parser_corpus.rs` | 76 filename cases from `fixtures/filenames.toml` |
| `mrn-core/tests/pipeline.rs` (12) | full TV + movie + anime (AniList→TMDB cross-ref) flow with nfo, execute, undo; uncertain match fail/ask/search; `--id` and `--offline`; provider order and cross-site ids; MAL fallbacks and episode titles without a TMDB key; later seasons via the sequel chain; manga falling back to MangaBaka; ComicInfo details then undo; Kavita embedded covers and Komga sidecars then undo |
| `mrn-cli/tests/cli.rs` (7) | real binary in isolated XDG dirs: rename/history/undo, `--conflict override` → trash → undo, `--yes` needed without a TTY, missing TMDB key message, config init/show masking, info, bad template, completions/man |
| `mrn-gui` `worker::tests` (1) | the GUI's job thread headless: offline preview → apply → undo, and cancel after preview leaves files alone |
| `mrn-gui` `snapshots` (ignored) | renders every screen with sample data to PNG (`MRN_GUI_SNAPSHOTS=dir cargo test -p mrn-gui -- --ignored`) |
