# Plan: more providers (MAL via Jikan, MangaBaka)

Written 2026-11-02 (planning session, nothing implemented yet). Read with
[DEVELOPMENT.md](DEVELOPMENT.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## Decisions

| Decision | Why |
|---|---|
| TMDB and AniList stay as they are | Already providers; TMDB live verification is still roadmap item 1 |
| MAL through **Jikan only** (`api.jikan.moe/v4`), not the official MAL v2 API | No key, and Jikan has episode titles; the official API has none |
| Add **MangaBaka** (`api.mangabaka.org/v1`) for manga | No key; one series carries AniList, MAL, MangaDex, MangaUpdates ids |
| **Provider order is user-configurable** per kind | User asked for it. Defaults keep today's results |
| Default manga order: AniList → MangaBaka → MangaDex | MangaBaka catches what AniList misses before the MangaDex fallback |
| Kavita metadata goes in ComicInfo: tags, creators, Count, LocalizedSeries, AgeRating | Kavita and Komga read these; the providers already have most of the data |
| Kavita: volume covers are embedded in the `.cbz` only with **`--embed-cover`** (off by default); an archive that already has a cover image (by Kavita's own rule) is skipped | Kavita only takes covers from inside the archive, and the added cover counts as an extra first page |
| Komga: volume covers are saved **beside** the book as `<stem>.<ext>` (with `--artwork` and `-f komga`); the archive is not touched | Komga reads such sidecars and prefers them over the first page. No extra page, and it works for `.cbr`/`.pdf` too |
| A chapter file gets the cover of its volume | MangaDex has covers per volume, not per chapter |

## Goals

1. Anime episode titles without a TMDB key, and for later-season AniList entries (known issue 6).
   AniList returns `idMal`, and MAL splits seasons like AniList, so the MAL id leads to the right
   episode list without matching by title.
2. Better manga and manhwa coverage, and cross-site ids for names, `.nfo` and ComicInfo.
3. Let users choose which provider is searched first for each kind.
4. Kavita/Komga get tags, creators and a cover for each volume and chapter.

## What the APIs give (to confirm before coding)

The APIs weren't reachable from the dev machine during planning, so the details below come from docs.
Step 1 of each phase is to fetch real responses and save them as test fixtures.

| | Jikan (MAL) | MangaBaka |
|---|---|---|
| Key | none | none |
| Search | `GET /v4/anime?q=`, `GET /v4/manga?q=` | `GET /v1/series/search` (check parameter names in the OpenAPI explorer at https://mangabaka.org/api) |
| Lookup | `GET /v4/anime/{id}`, `/v4/manga/{id}` | series by id (check the path) |
| Episodes | `GET /v4/anime/{id}/episodes`, paged (`pagination.has_next_page`) | n/a |
| Rate limit | about 3 per second, 60 per minute (sources disagree on the per-second cap) | search 30 per minute per IP; cached responses don't count; 429 when over |
| Notes | unofficial scraper of MAL; can be slow or briefly down | weekly data refresh; own data CC BY-NC-SA 4.0 |

### How Kavita and Komga pick covers (checked in their source, 2026-11-02)

**Kavita** (`Kavita.Services/ArchiveService.cs`, `Scanner/Parser.cs`, `develop` branch):
- `FindCoverImageFilename`: sort the archive's images naturally by name, take the first whose
  file name matches `CoverImageRegex`; otherwise the first image (`FirstFileEntry`).
- `CoverImageRegex`:
  `(?<!back[\s_-])(?<!\(back )(?<!back)(?:^|[^a-zA-Z0-9])(!?cover|folder)(?![a-zA-Z0-9]|s\b)`,
  case-insensitive. `!cover.jpg`, `cover.png` and `folder.jpg` match; `discovery.jpg`, `covers.jpg`
  and `back cover.jpg` don't.
- `GetNumberOfPagesFromArchive` counts every image, so an embedded cover is an extra page.
- No sidecar covers: images named like a cover in library folders are skipped by the scanner
  (`IsSkippedCoverImage`), and no code reads a series-folder `cover.jpg`. The series cover comes
  from its first volume/chapter.

**Komga** (`BookAnalyzer.kt`, `ZipExtractor.kt`, `LocalArtworkProvider.kt`, `master` branch):
- Generated cover: the first image of the book, sorted case-insensitively and naturally. File
  names have no special meaning.
- Sidecars, when the library's "Import local artwork" is on (the default):
  - Book: an image beside the book whose base name equals the book's, optionally with `-N`
    (`Vol.01.jpg`, `Vol.01-1.jpg` next to `Vol.01.cbz`). Extensions: png, jpeg, jpg, tbn, webp, gif.
  - Series: `cover`, `default`, `folder`, `poster` or `series` with those extensions, in the series
    folder. mrn's existing `cover.<ext>` already works.
  - A sidecar is selected when the current cover is generated or missing
    (`MarkSelectedPreference.IF_NONE_OR_GENERATED`).

Still to confirm before Phases 3 and 4 (save real responses as fixtures):

| What | Check |
|---|---|
| Kavita ComicInfo fields | which ones it reads (Tags, Writer, Penciller, Count, LocalizedSeries, AgeRating) |
| Kavita and loose images | whether a `<stem>.jpg` beside a `.cbz` in a manga/comic library becomes an image chapter. If not, book sidecars can be written for every preset, not just `komga` |
| MangaDex covers | `GET /cover?manga[]=ID&order[volume]=asc` shape, paging, `volume` and `locale` fields |
| MangaDex chapter → volume | `GET /manga/{id}/aggregate` shape |

## Phase 0: groundwork (no behaviour change)

1. **`Source`** (`providers/mod.rs`): add `Mal` and `Mangabaka` to `as_str`/`FromStr`
   (`mal`, `myanimelist`; `mangabaka`, `mb`) and to the error text. `--id mal:52991` then works.
2. **Cross ids:** `Candidate.ext_ids: BTreeMap<Source, String>`. AniList fills `Mal` from `idMal`
   (add it to the GraphQL query). MangaBaka fills every id it has.
3. **Configurable order** replaces the hard-coded `Providers::for_kind`:
   ```toml
   [providers]
   # Searched in this order; the first confident match wins.
   anime = ["anilist", "mal"]
   manga = ["anilist", "mangabaka", "mangadex"]
   # Where anime episode titles ({t}) come from, in order.
   anime_episodes = ["tmdb", "mal"]
   ```
   - Validate at load: unknown names are an error; a source that can't serve the kind
     (e.g. `comicvine` under anime) is an error; a duplicate is an error.
   - A keyed source without a key is skipped silently (as now), so `missing_reason` still works.
   - Movie, TV and comic keep one provider each, so leave them out of `[providers]`.
   - Per-run override: `--providers anime=mal,anilist` (repeatable), stored in `Options` so the GUI
     can set it too.
   - Add the section to `config_template.toml` and `mrn config show`.
4. **Template fields:** `malid` and `mangabakaid` next to `tmdbid`/`anilistid` (`KNOWN_FIELDS` in
   `format/template.rs`, `build_fields` in `pipeline.rs`). Fill them from the matched source and
   from `ext_ids`, so an AniList match can still render `{malid}`.
5. **`.nfo`:** one `<uniqueid type="…">` per known id; the matched source is `default="true"`.
6. **Rate limits** (`providers/http.rs`): `api.jikan.moe` 3 burst / 1 per second;
   `api.mangabaka.org` 5 burst / 0.5 per second.

## Phase 1: MAL through Jikan

- `providers/mal.rs`, `Mal::new(http, anime_title)` plus `with_base_url()`.
- `search`/`lookup` for anime and manga. Title follows `general.anime_title`: `title` (romaji),
  `title_english`, `title_japanese`. All of `titles[]` go to `alt_titles`. Year from `aired.from` or
  `published.from`. Poster from `images.jpg.large_image_url`. Drop light novels from manga search,
  as AniList does.
- `series()`: page through `/episodes`. Each item becomes `Episode { season: 0, episode: n,
  absolute: Some(n), title, air_date }`, so anime falls back to the absolute templates when titles
  come from MAL.
- `Engine::series_for` walks `anime_episodes`:
  - `tmdb`: today's cross-reference by title.
  - `mal`: the MAL id from the match (`Source::Mal`) or `ext_ids[Mal]` (AniList match). No title search.
  - The first source that returns episodes wins.
- **Known gap:** some release groups continue numbering from the previous season (Frieren S2 as
  29, 30, …). If the parsed number is beyond the entry's episode count, give no title (as now).
  The proper fix follows the MAL/AniList prequel chain to offset the numbers. Out of scope; note it
  under issue 6.

## Phase 2: MangaBaka

- `providers/mangabaka.rs` plus `with_base_url()`: `search`, `lookup`, `ext_ids` from the series'
  source ids. Filter out novels the same way.
- Slot it into the default manga order (AniList → MangaBaka → MangaDex).
- Optional: use MangaBaka's ids/genres to enrich ComicInfo (`Web`, `Genre`).

## Phase 3: richer ComicInfo (tags, creators)

- `Provider::book_details(&Candidate) -> Result<BookDetails>`, empty by default like `series()` and
  `issue()`. Called once per matched book series, only when `--comicinfo` or `--embed-cover` is on.
- `BookDetails { tags, credits: Vec<(role, name)>, count, age_rating, volume_covers: volume → URL,
  chapter_volumes: chapter → volume }`. `count` only when the series is finished.
- **AniList:** a separate lookup (search results stay small) with `tags { name rank isMediaSpoiler }`,
  `staff(perPage: 8) { edges { role node { name { full } } } }`, `status volumes chapters isAdult`.
  Drop spoiler tags and tags ranked below 60. Roles: Story → Writer, Art → Penciller,
  "Story & Art" → both. `isAdult` → AgeRating "Adults Only 18+".
- **MangaDex:** tags in group `theme`/`format` → Tags, `genre` → Genre. `contentRating` → AgeRating
  (safe → Everyone, suggestive → Teen, erotica → Mature 17+, pornographic → Adults Only 18+).
  Author and artist from `includes[]=author&includes[]=artist`. `lastVolume` → Count when completed.
- **MangaBaka:** use its tags and genres if it has them (check in Phase 2).
- **ComicVine:** issue credits need one more request per issue at 200 per hour. Out of scope; a known
  limitation.
- `comicinfo::build` (`extras/comicinfo.rs:19`) takes `Option<&BookDetails>` and writes `Tags`,
  `Writer`, `Penciller`, `Count`, `AgeRating`, and `LocalizedSeries` (`english_title` when it differs
  from `title`). Deduplicate genres and tags.

## Phase 4: volume and chapter covers (`--embed-cover` for Kavita, sidecars for Komga)

- **Sources:**
  - Comics: `Issue.cover` from ComicVine (`providers/mod.rs:164`), already fetched and never used.
  - Manga: MangaDex `GET /cover?manga[]=ID&order[volume]=asc`, paged. Prefer the original-language
    locale, else any.
  - AniList or MangaBaka match: the MangaDex id from `ext_ids[Mangadex]` (Phases 0 and 2). Otherwise
    cross-reference by title, as TMDB is for anime; skip quietly when nothing matches.
- **Which volume:** the parsed `info.volume`; else the chapter's volume from
  `GET /manga/{id}/aggregate` (one request per series); else no cover.
- **Komga, sidecar** (`--artwork` with `-f komga`): write `<target stem>.<ext>` beside the book
  through the existing artwork path in `extras/mod.rs` (it already records created files in history
  and never overwrites). Works for every book format and every action, since the book is untouched.
- **Kavita, embedded** (`--embed-cover`), in `extras/comicinfo.rs`:
  - Generalise `rewrite` to set or remove a named entry.
  - `embed_cover(archive, bytes, ext) -> Result<Option<String>>` skips the archive when any image's
    file name matches a port of Kavita's `CoverImageRegex` (above), not a plain "contains cover".
    Otherwise it adds `!cover.<ext>` and returns the entry name. `!` sorts before digits and letters,
    so the cover is also page 1 in readers.
  - `remove_entry(archive, name)` for undo.
  - The `regex` crate has no look-behind: check the three `back` exclusions by hand on the text
    before the match.
- **History:** `OpKind::Cover` (`"cover"`), `data` = the entry name; undo calls `remove_entry`.
  Where `history.rs` special-cases `Embed` (batch counts, in-place revert), use one helper
  `OpKind::edits_in_place()` covering `Embed` and `Cover`.
- **Pipeline:** `embed_cover` next to `embed_comicinfo` (`pipeline.rs:699`) with the same guards
  (move or copy only, `.cbz`/`.zip` only). Runs after ComicInfo; every change goes through
  `start_op` → `finish_*`/`fail_op`. Download through `Http` so the cache and rate limiter apply.
- `ExtrasOptions.embed_cover`, CLI `--embed-cover`, config key next to `comicinfo`.

## Phase 5: front ends and docs

- CLI: `--providers`, `--embed-cover`; `mrn info` already prints the source per candidate (check it
  shows the new ones).
- GUI: no key fields needed. Provider order joins the "edit Settings" follow-up (reorderable list per
  kind); until then it comes from the config file. An "embed volume covers" checkbox next to ComicInfo.
- Docs: ARCHITECTURE.md providers table, rate limits, extras table and history ops; DEVELOPMENT.md
  (narrow issue 6, issue 9 covers `.cbz`-only embedded covers too, ComicVine credits, roadmap and
  decision log); README config and extras sections (which cover method suits Kavita vs Komga) and
  the `--id` sources.

## Tests

- Unit tests (wiremock) per provider: search, lookup, novel filtering, Jikan paging, a 429 retry.
- Config: order parsing, invalid names, wrong kind, duplicates, `--providers` override.
- Pipeline test with **no TMDB key**: AniList match with `idMal` → Jikan episodes →
  `Sousou no Frieren - 12 - <title>.mkv`; and `anime_episodes = ["mal", "tmdb"]` picks MAL first.
- Pipeline test: manga with AniList failing over to MangaBaka.
- wiremock tests for AniList and MangaDex `book_details`, and the cover and aggregate responses.
- `comicinfo::build` writes Tags, Writer, Count and LocalizedSeries.
- `embed_cover`: an archive with no cover gets `!cover.jpg` as its first entry; one with `cover.png`
  is left alone; one with only `discovery.jpg` or `back cover.jpg` still gets a cover; undo removes
  the entry and keeps the mtime. Table-test the regex port with Kavita's own `ParsingTests` cases.
- Sidecar: `-f komga --artwork` writes `Vol.01.jpg` beside `Vol.01.cbz`, skips an existing one, and
  undo removes it.
- History: ComicInfo plus a cover in one batch, both undone.
- Live checks (no keys needed): Frieren S1 and S2, an anime absent from TMDB, Solo Leveling
  (manhwa) via MangaBaka, an episode number past the entry's count, a Kavita library scan showing
  tags, creators and embedded volume covers, and a Komga scan showing sidecar volume covers.

## Order of work

Phase 0 → 1 → 2 → 3 (richer ComicInfo) → 4 (covers) → 5 (front ends and docs), one commit each, then
a release. Phase 1 is the most valuable; Phase 2 can wait if time runs short. Phases 3 and 4 need
`ext_ids` from Phase 0; Phase 4 works best after Phase 2 (MangaBaka's MangaDex ids).
