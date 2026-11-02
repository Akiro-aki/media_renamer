# Plan: more providers (MAL via Jikan, MangaBaka)

Written 2026-11-02. Read with [DEVELOPMENT.md](DEVELOPMENT.md) and [ARCHITECTURE.md](ARCHITECTURE.md).
Work happens on the `providers` branch.

**Progress:**
- **Phase 0: done (2026-11-02).**
  - `Source::Mal`/`Mangabaka` exist with aliases (`myanimelist`, `mb`), but they have no provider
    yet. `Providers::get` returns `None` for them, so they are skipped in search lists, and
    `--id mal:N` fails with "not supported yet" until Phase 1.
  - `[providers]` is `config::ProviderOrder` (validated in `Config::load`). `--providers LIST=SOURCES`
    parses to `ProviderSpec` and is stored in `Options.providers`.
  - `Engine::series_for` walks `anime_episodes`. Its `mal` arm is a stub until Phase 1.
  - Defaults keep today's behaviour (`anime = ["anilist"]`, `manga = ["anilist", "mangadex"]`,
    `anime_episodes = ["tmdb"]`). Phases 1 and 2 add `mal`/`mangabaka` to them.
  - Besides AniList's `idMal`, MangaDex's `links` (`al`, `mal`) also fill `ext_ids`.
- **Phase 1: done (2026-11-02), not yet checked against the live API.**
  - `providers/mal.rs` is built from Jikan's OpenAPI spec (`jikan-me/jikan-rest`,
    `storage/api-docs/api-docs.json`) and its scraper (`jikan-me/jikan`). The live API was
    unreachable from the dev sandbox.
  - Facts confirmed in that source: an episode's `mal_id` is its episode number (MAL's
    episode-number column); episode pages hold 100.
  - MAL episodes are season 1 with `absolute` = number, and the list is `SeriesInfo.single_entry`.
    Season 0 would mean specials in the TV templates.
  - **Later seasons** (known issue 6). A search usually matches season 1's entry, so for a file
    that needs another season (`S02E03`, or numbering past the entry's end such as `- 31`),
    `Engine::mal_series` finds the franchise's seasons. `Anilist::season_chain` follows `PREQUEL`
    and `SEQUEL` links:
    - TV, TV_SHORT and ONA entries count as seasons. Movies and specials are passed through.
    - An entry titled like the previous one plus "Part 2", "Cour 2" or "2nd Cour" continues that
      season (`base_title`).
    - The seasons' MAL lists are joined, using TheTVDB-style season numbers and absolute numbers
      counted from the matched entry. Only the seasons the files need are fetched.
    - Without a usable chain, a MAL list stays `single_entry` and names only season 1 (or
      unnumbered) files: no title rather than a wrong one.
  - `anime_episodes` sources now **merge**: each later source fills in episodes the earlier ones
    lack, until every file finds its episode. With a TMDB key, TMDB names what it has and MAL
    fills the gaps.
  - The chain was checked live against AniList (`live_season_chain`, ignored by default): Frieren
    S1 → S2 → S3; Attack on Titan gives 4 seasons, with "Season 3 Part 2" and "The Final Season
    Part 2" continuing their seasons, as on TheTVDB.
  - Considered and not chosen: the Fribb/anime-lists mapping (AniDB-based, about 6 MB, maps each
    entry to its TheTVDB season and offset) and the AniDB API (registered client, 1 request per 2
    s with bans, splits seasons like AniList).
  - Defaults are now `anime = ["anilist", "mal"]` and `anime_episodes = ["tmdb", "mal"]`.
  - To do: live Jikan checks (Frieren S1 and S2, an anime missing from TMDB) and a real Jikan
    response saved for the tests.
- **Phase 2: done (2026-11-02), checked against the live API.**
  - `providers/mangabaka.rs`, built from the OpenAPI spec (`https://mangabaka.org/api.json`;
    `api.mangabaka.org/api.json` is a 404) and real responses. Trimmed real responses are test
    fixtures in `tests/fixtures/mangabaka_*.json`.
  - Search: `GET /v1/series/search?q=&type_not=novel&limit=10`. Lookup: `GET /v1/series/{id}`;
    a `merged` series is followed once to `merged_with`, a `deleted` one is an error.
  - Titles: `title` is the main (usually English) title. `romanized_title` is unreliable
    (Frieren's is the Turkish "Frieren Cenazede"), so romaji is the primary `titles[]` entry in
    `<original_language>-Latn` (`ja-Latn`, `ko-Latn`). English is the primary `en` title. Titles
    starting with "unknown title (please report on Discord)" are placeholders and are skipped.
  - `source` holds AniList, MAL, MangaUpdates, Kitsu, Anime-Planet, ANN and Shikimori ids; AniList
    and MAL go to `ext_ids`. **MangaBaka has no MangaDex ids**, so Phase 4 can't get them from it
    (only from a MangaDex match or a title cross-reference).
  - `genres` are slugs (`slice_of_life`), turned into MangaBaka's labels (`Slice of Life`). Some
    entries send `null` for lists (`genres`, `tags`). `tags` (strings) and `tags_v2` (with
    `is_spoiler`, `weight`) exist for Phase 3.
  - Live dry run: Solo Leveling, Sousou no Frieren and Omniscient Reader's Viewpoint all matched at
    100% with the right AniList and MAL ids.
  - Default manga order is now `["anilist", "mangabaka", "mangadex"]`.
- Phase 3: next.

## Decisions

| Decision | Why |
|---|---|
| TMDB and AniList stay as they are | Already providers; TMDB live verification is still roadmap item 1 |
| MAL through **Jikan only** (`api.jikan.moe/v4`), not the official MAL v2 API | No key, and Jikan has episode titles; the official API has none |
| Add **MangaBaka** (`api.mangabaka.org/v1`) for manga | No key; one series carries AniList, MAL, MangaUpdates and Kitsu ids (not MangaDex, it turned out) |
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
- **Later seasons** (`S02E03`, or Frieren S2 numbered on from S1 as 29, 30, …): done through
  the AniList sequel chain (see Progress above).

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
- **MangaBaka:** `tags` (names) and `tags_v2` (`is_spoiler`, `weight`, `name_path`); `genres`
  slugs; `authors`/`artists` name lists; `status`, `final_volume`, `content_rating` (same values
  as MangaDex). All are in the search and lookup responses already.
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
  - AniList match: the MangaDex id from `ext_ids[Mangadex]` if a provider supplied it (MangaBaka
    doesn't; checked in Phase 2). Otherwise
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
`ext_ids` from Phase 0; Phase 4 can't use MangaBaka for MangaDex ids (it has none).
