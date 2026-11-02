# Development log and handoff

Last updated: **2026-11-02** (sessions 2–5: AppImage packaging, small fixes, the GUI, complete undo, more providers). Start here when
you pick the project up again. Architecture details are in
[ARCHITECTURE.md](ARCHITECTURE.md).

## Goal

A free, open alternative to FileBot for Linux: match media file names against
online databases and rename/move them into Jellyfin/Plex/Kodi/Kavita/Komga
layouts. Agreed scope for v1:
- **Rust**, **CLI first**. The core is a library so a GUI can come later.
- Media: **TV, movies, anime, comics, manga**.
- Extras: **undo/history** (always on) and **artwork + .nfo + ComicInfo.xml** (opt-in, off by default).
- Offered but **not chosen for v1:** subtitle download, watch-folder automation, music.

The original plan is archived in [PLAN-v1.md](PLAN-v1.md).

## Status

All seven build steps from the plan are done:

| # | Step | State |
|---|------|-------|
| 1 | Workspace, git init, config | done |
| 2 | Parser + detection + corpus, `mrn info` offline | done (76 corpus cases; the plan aimed for about 150) |
| 3 | Templates, ops, history; rename/dry-run/undo end to end | done |
| 4 | TMDB + cache + rate limiter + matcher | done (mocked tests only, see below) |
| 5 | AniList, MangaDex, ComicVine; later MAL (Jikan) and MangaBaka | done (AniList, MangaDex, MangaBaka verified live; Jikan and ComicVine not yet) |
| 6 | Extras: nfo, artwork, ComicInfo | done |
| 7 | Interactive picker, `--non-interactive`, README | done |

### Session 2 (2026-11-01)

- **AppImage packaging** in `packaging/appimage/` (`build.sh`, `AppRun`, `mrn.desktop`, `mrn.svg`/`mrn.png`).
  The AppDir bundles the binary, man page, bash/zsh/fish completions and the README. `build.sh --container`
  builds the binaries in `rust:1-bookworm`; the result needs glibc 2.35 (a host build on CachyOS needs 2.44).
- `mrn completions <shell>` and `mrn man` (clap_complete, clap_mangen).
- Fixed known issue 1: embedded ComicInfo.xml now carries the current UTC time (no date crate; see `comicinfo::zip_time`).
- Fixed known issue 8: AniList search drops `format: NOVEL` entries; `--id anilist:N` still reaches a novel.
- Verified the cross-filesystem move (former "not verified" item 4), see below.
- Released as `v0.1.0` on GitHub (Akiro-aki/media_renamer) with the CLI-only AppImage.

### Session 3 (2026-11-01): the GUI, released as v0.2.0

- New crate `crates/mrn-gui` (bin `mrn-gui`, iced 0.14, chosen by the user over GTK4/egui for
  easy AppImage packaging). Tabs: Rename (sources, options, preview, uncertain-match picker,
  apply with progress, undo), History (batches with undo, "undo anyway" for changed files),
  Settings (API keys, open config). Design notes in ARCHITECTURE.md ("The GUI").
- Core additions so both front ends share logic: `preview.rs` (`Preview`, rows, `relative`, `ago`),
  `Action::verbs`, `Config::load_file` (no env overrides), `config::save_api_keys` (toml_edit).
  The CLI preview now prints from `Preview`.
- `mrn gui [PATHS]` execs the sibling or `PATH` `mrn-gui`. The AppImage's `AppRun` opens the GUI
  when started with no arguments and no terminal; the desktop entry is now the GUI's.
- Release builds should use `build.sh --container`: on CachyOS the GUI binary needs glibc 2.44
  (libm `cosh`/`sinh` and other maths symbols are re-versioned in new glibc).

### Session 4 (2026-11-01): complete undo, chain renames (unreleased)

- Fixed known issue 3: `--conflict override` moves the replaced file to the freedesktop.org trash on the same
  file system (new `trash.rs`), records the trash path in history, and `undo` puts it back.
- Fixed known issue 2: ComicInfo embedding is recorded as an `embed` op holding the previous entry;
  `undo` rewrites the archive back (`comicinfo::restore`). Embedding now keeps the archive's mtime.
- Fixed known issue 4: chain renames in one batch (A→B while B→C) are planned and ordered; name swaps are skipped.
- History schema: new `ops.backup` and `ops.data` columns, added in place to existing databases.
- `build_plan` now takes the `Action`; `ops::apply` takes `Option<&Trash>` and returns `Applied`;
  `History::finish_op` takes the replaced file's trash path instead of a bool.

### Session 5 (2026-11-02): more providers, richer ComicInfo, covers (branch `providers`)

Plan and per-phase notes: [PLAN-providers.md](PLAN-providers.md). One commit per phase.
- Phase 0: configurable provider order (`[providers]`, `--providers`), cross-site ids
  (`Candidate.ext_ids`, `{malid}`, `{mangabakaid}`, one `.nfo` `<uniqueid>` per id).
- Phase 1: MAL through Jikan for anime search and episode titles without a TMDB key; later anime
  seasons through AniList's sequel chain (known issue 6).
- `http.rs`: a host that fails every attempt with connection errors is skipped for 5 minutes, and
  there is no backoff after the last attempt (found when Jikan was down).
- Phase 2: MangaBaka for manga (default order AniList → MangaBaka → MangaDex).
- Phase 3: ComicInfo gets Tags, Writer, Penciller, Count, AgeRating and LocalizedSeries from
  `Provider::book_details` (AniList, MangaDex, MangaBaka), as Kavita and Komga read them.
- Phase 4: volume covers. `--embed-cover` adds `!cover.<ext>` inside each `.cbz` for Kavita;
  `--artwork -f komga` saves `<stem>.<ext>` beside each book for Komga. New history op `cover`.
- Phase 5: GUI "Covers in .cbz (Kavita)" toggle and a read-only provider order in Settings; `--id`
  help lists every source; docs.

### Verified

- `cargo test --workspace` passes: 69 core unit tests (plus 1 ignored live AniList test), the corpus (76 cases), 12 pipeline integration tests, 7 CLI tests and 1 GUI worker test (plus an ignored GUI snapshot test).
- **Override → trash → undo on NTFS** (2026-11-01): debug binary, library on the fuseblk `/mnt/shared` mount.
  The replaced file went to `/mnt/shared/.Trash-1000` with a mount-relative `Path=`; undo restored both files and
  removed the trash entry. The home-trash path is covered by a CLI test.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- Manual offline run on dummy files: dry run, real move with `--clean`, `history`, and `undo`. Undo restored everything, including folders pruned by `--clean`, and removed the created library folders.
- **Live AniList:** `[SubsPlease] Sousou no Frieren - 12` → anilist:154587; One Piece and Chainsaw Man manga matched at 100%.
- **Live extras:** the manga cover downloaded as a real PNG, ComicInfo.xml was embedded in a `.cbz` (checked with `unzip`), and undo removed the cover.
- Without a TMDB key, movies/TV fail with a clear message and exit code 1.
- **Live providers branch** (2026-11-02): MangaBaka search and lookup (Solo Leveling, Frieren,
  Omniscient Reader's Viewpoint); ComicInfo tags/creators/Count/AgeRating from AniList, MangaBaka and
  MangaDex; embedded covers in three `.cbz` files and a Komga sidecar, then `mrn undo`; the AniList
  sequel chain (Frieren, Attack on Titan). Kavita's and Komga's ComicInfo and cover rules were read in
  their source (see PLAN-providers.md).
- **Cross-filesystem move** (2026-11-01): release binary, NTFS fuseblk `/mnt/shared` → tmpfs library with
  `--clean`, then `mrn undo`. Video and sidecar moved and came back with identical SHA-256; undo removed the created library folders.
- **AppImage** (2026-11-01): `packaging/appimage/build.sh` produced `dist/mrn-0.1.0-x86_64.AppImage` (6.1 MB,
  appimagetool continuous build 340, downloaded by the script). Installed as `mrn` on a temp `PATH`, it ran an offline
  rename into a library and `undo` restored it; `--version` also works with `APPIMAGE_EXTRACT_AND_RUN=1`.

### Not verified yet

1. **TMDB against the real API** (movies, TV, and anime episode titles). No key was available, so only wiremock tests cover it. Response shapes follow the TMDB v3 docs.
2. **ComicVine against the real API** (needs a key). Same situation.
3. **The interactive picker** (`ui.rs::prompt`) in a real terminal. Only the `Chooser` logic is tested, with a scripted chooser.
4. **The GUI by hand.** Checked: headless snapshots of every screen (dark and light), the worker
   test (preview → apply → undo, cancel), and that the window opens on Hyprland from the
   container-built AppImage via `mrn gui` and via a no-terminal launch. Not checked: the
   portal file dialogs, a real uncertain-match pick, and X11 drag and drop.
5. The AppImage hasn't been tried on another distro.
6. `hardlink` and `symlink` were unit-tested in a tempdir only, never through the CLI on real media.
7. **Jikan (MAL) against the real API.** `api.jikan.moe` was unreachable in both sessions (it refused
   connections on 2026-11-02 while other hosts worked); only wiremock tests built from Jikan's
   OpenAPI spec cover it.
8. **A real Kavita and Komga scan** showing the new ComicInfo fields and the covers.

## Resuming work

```sh
cd /mnt/shared/dev/Projects/media_renamer
cargo build                       # first build ~2.5 min: the repo is on an NTFS (fuseblk) mount
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p mrn-cli -- --help
```

- **Test in isolation.** A normal `mrn` run writes to your real `~/.local/share/mrn/history.db` and `~/.cache/mrn`. For experiments, point XDG at a temp dir:
  ```sh
  T=$(mktemp -d); export XDG_CONFIG_HOME=$T/c XDG_DATA_HOME=$T/d XDG_CACHE_HOME=$T/k
  ```
  The CLI tests do the same (see `mrn-cli/tests/cli.rs`).
- **Dummy media:** `truncate -s 1k "Name.S01E01.mkv"`. For a valid `.cbz`, run `python3 -c "import zipfile; z=zipfile.ZipFile('X v01.cbz','w'); z.writestr('001.jpg', b'x'); z.close()"`.
- **TMDB key:** get a free one at https://www.themoviedb.org/settings/api, then `export TMDB_API_KEY=...`. Do verification item 1 first.
- **Faster builds:** put the target dir on a Linux filesystem, e.g. `export CARGO_TARGET_DIR=~/.cache/cargo-target/media_renamer`.
  `/home` is small: the debug dir there grew to 14 GB and filled the disk in session 4. If a build fails with
  "No space left on device", `rm -rf ~/.cache/cargo-target/media_renamer/debug` (it rebuilds in a few minutes).
- **Git:** `master` on https://github.com/Akiro-aki/media_renamer. Tags `v0.1.0` (CLI) and `v0.2.0` (GUI), both released with an AppImage. Build release AppImages with `packaging/appimage/build.sh --container`.

## Conventions

- Edition 2024 (let-chains are used), MSRV `rust-version = "1.89"` in the workspace `Cargo.toml`. Toolchain used: rustc 1.99.
- Errors: `anyhow` everywhere. Provider and HTTP errors never include URLs (API keys).
- Every provider has `with_base_url()` so tests can point it at wiremock. Keep that when adding providers.
- **Parser changes:** add the failing name to `tests/fixtures/filenames.toml` first, then fix. The corpus test lists every mismatch at once.
- Keep the CLI thin. New behaviour belongs in `mrn-core` (usually `pipeline.rs`) so the GUI gets it too.
- Never overwrite user files from extras. Never write series-level extras outside a library root.
- Every disk change must go through `history` (`start_op` before, `finish_op`/`fail_op` after) so undo stays complete.

## Known issues and limitations

| # | Issue | Where | Notes |
|---|-------|-------|-------|
| 1 | ~~Embedded `ComicInfo.xml` entries are dated 1980~~ | `extras/comicinfo.rs` | fixed 2026-11-01 |
| 2 | ~~Undo doesn't remove ComicInfo.xml from a cbz it moves back~~ | `history.rs` | fixed 2026-11-01: `embed` op with the previous entry |
| 3 | ~~A file replaced with `--conflict override` can't be restored~~ | `ops.rs`, `history.rs` | fixed 2026-11-01: replaced files go to the trash. Batches recorded before still can't restore them |
| 4 | ~~Chain renames in one batch (A→B while B→C) are treated as conflicts~~ | `ops::build_plan` | fixed 2026-11-01. Name swaps (A↔B) are still skipped; they would need a temporary name |
| 5 | Files of one show with and without a year in the name form two lookup groups (two prompts) | `pipeline.rs` step 2 | could merge groups by title and resolve the year after |
| 6 | Anime episode titles come from TMDB (title cross-reference) or MAL (by id). Later seasons are found through AniList's sequel chain; without a usable chain a MAL list names only season 1 | `Engine::series_for`, `Engine::mal_series` | narrowed 2026-11-02. A franchise with unusual splits may still number differently from TheTVDB |
| 7 | ComicVine allows 200 requests per hour, so big comic batches with `{t}`/`--comicinfo` are slow | `providers/http.rs` | the cache helps on reruns |
| 8 | ~~AniList manga search can return light novels~~ | `providers/anilist.rs` | fixed 2026-11-01: novels are filtered from search; light-novel `.epub` files need `--id anilist:N` |
| 9 | ComicInfo and covers are only embedded in `.cbz`/`.zip`, not `.cbr`, `.cb7`, `.pdf` or `.epub` | `extras/comicinfo.rs`, `extras/cover.rs` | RAR can't be written. Komga sidecars work for every format |
| 10 | Parser heuristics have edge cases: a bare `Episode N` without a season is treated as anime; `S2` in a title is read as a season; the book `c`/`v` prefixes can misfire | `parse/` | grow the corpus with real names |
| 11 | Spinner and progress bar are hidden without a TTY; applying changes non-interactively requires `--yes` | `mrn-cli/src/ui.rs`, `rename.rs` | deliberate |
| 12 | `--clean` only removes folders that end up completely empty (leftover `.txt`/samples keep them) | `ops::prune_empty_dirs` | deliberate, safe |
| 13 | ComicVine creators aren't written to ComicInfo | `providers/comicvine.rs` | they need one more request per issue at 200 per hour |
| 14 | Chapter files only get a cover when MangaDex knows the chapter's volume | `Mangadex::volume_covers` | licensed series list few chapters there (Frieren: 1-2); misfiled chapters are ignored |
| 15 | Provider data is written as is, e.g. MangaDex calls Chainsaw Man completed at 232 chapters (Part 1), so Count is 232 | `providers/` | deliberate |

## Roadmap (suggested order)

**Next (finish v1):**
1. Verify TMDB live with a real key: movies, TV, daily shows, and anime cross-reference. Fix any response-shape surprises.
2. Verify ComicVine live (key). Run `packaging/appimage/build.sh --container` and try the result on an older distro.
3. Grow the corpus toward 150+ using real names from your library.
4. Write a PKGBUILD (Arch/CachyOS).
5. Verify Jikan live (Frieren S1 and S2, an anime missing from TMDB) and save a real response as a fixture.
6. Scan a test library in Kavita and in Komga: tags, creators, Count, age rating, embedded covers and sidecars.

**Then:**
- `--json` output for `rename -n`, `info` and `history` (scripting, GUI prototyping).
- Episode ordering options (DVD/absolute) and a per-run title language.
- MangaDex chapter titles for `{t}` on manga.

**GUI follow-ups:**
- Per-row exclude checkboxes and per-row "match again" in the preview.
- Editing library folders, templates and the provider order (a reorderable list per kind) in
  Settings (now API keys, with the provider order shown read-only).
- Remember the last sources/options between runs.
- Wayland drag and drop once winit supports it.

**Later (v2):**
- Watch-folder automation (a systemd user unit or torrent-client hook running `mrn rename --non-interactive -y`).
- Subtitle download (OpenSubtitles hash lookup).
- More providers: TheTVDB (paid key), AniDB (strict UDP API), MusicBrainz for music.

## Decision log

| Decision | Why |
|---|---|
| Rust, cargo workspace (`mrn-core` + `mrn-cli`) | User choice; a single binary; the core is reusable for a GUI |
| Hand-written regex parser and corpus, not a guessit port | No mature Rust guessit; the corpus keeps the behaviour pinned |
| TMDB for TV (not TheTVDB) | TheTVDB v4 needs a paid key; TMDB is free and also covers movies |
| AniList for anime identity, TMDB for episode titles | AniList matches romaji fansub names best but has no episode titles |
| MangaDex as manga fallback, ComicVine for comics | Free; ComicVine is the standard for western comics |
| Own template language instead of Groovy | Simple, safe, validated at parse time; optional sections cover most needs |
| In-place rename when there is no output root | Matches FileBot; avoids creating folder trees in Downloads |
| Default preset `jellyfin`, `windows_safe = true` | Common target; the media lives on an NTFS mount and may be shared over SMB |
| Extras off by default | User asked for artwork to be optional |
| SQLite (rusqlite, bundled) for cache and history | Atomic, inspectable, no system dependency |
| Dropped quick-xml | nfo/ComicInfo are flat; a 30-line escaper is enough |
| reqwest with rustls | No OpenSSL dependency |
| Jaro-Winkler + Levenshtein average | Jaro-Winkler alone matched "Dune" to "Dune: Part Two" |
| Manual search keeps the file's year | Without it, "Dune (1984)" outranked "Dune (2021)" |
| AppImage as the binary package | User asked for it |
| iced for the GUI, file dialogs via the XDG portal | User's choice; pure Rust, so the AppImage needs no bundled GTK/Qt; the portal follows the desktop's own dialog |
| GUI work on a dedicated thread per job | `Chooser::choose` is synchronous and blocks until the user answers |
| AppImage built with `APPIMAGE_EXTRACT_AND_RUN=1` and `--no-appstream` | Works without FUSE in containers/CI; no AppStream id is settled yet |
| `mrn.png` committed next to `mrn.svg` | appimagetool rejects an SVG `.DirIcon`; a committed PNG keeps the build free of rsvg/ImageMagick |
| ComicInfo timestamp computed by hand | The zip `time` feature would pull in the `time` crate for one call |
| MAL through Jikan, not the official MAL API | No key, and Jikan has episode titles |
| MangaBaka between AniList and MangaDex | No key; carries AniList and MAL ids (but no MangaDex ids) |
| Provider order configurable per kind | User asked for it; defaults kept the old results |
| Later anime seasons via AniList `PREQUEL`/`SEQUEL` | No mapping database to ship; matches TheTVDB seasons for checked franchises |
| Kavita covers embedded (`--embed-cover`), Komga covers as sidecars (`-f komga --artwork`) | Kavita only reads covers inside archives and would scan a loose image as a chapter; Komga prefers sidecars and the archive stays untouched |
| Cover-name rule ported from Kavita's `CoverImageRegex` by hand | The `regex` crate has no look-behind; tested with Kavita's own cases |
| Skip a host for 5 minutes after it fails every connect | A down provider (Jikan) cost about 55 s per request |
