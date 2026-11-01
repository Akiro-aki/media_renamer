# Development log and handoff

Last updated: **2026-11-01** (second session: AppImage packaging, small fixes). Start here when
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
| 5 | AniList, MangaDex, ComicVine | done (AniList/MangaDex verified live) |
| 6 | Extras: nfo, artwork, ComicInfo | done |
| 7 | Interactive picker, `--non-interactive`, README | done |

### Session 2 (2026-11-01)

- **AppImage packaging** in `packaging/appimage/` (`build.sh`, `AppRun`, `mrn.desktop`, `mrn.svg`/`mrn.png`).
  The AppDir bundles the binary, man page, bash/zsh/fish completions and the README. `build.sh --container`
  builds the binary in `rust:1-bookworm` for glibc 2.36; a host build on CachyOS needs glibc 2.39.
- `mrn completions <shell>` and `mrn man` (clap_complete, clap_mangen).
- Fixed known issue 1: embedded ComicInfo.xml now carries the current UTC time (no date crate; see `comicinfo::zip_time`).
- Fixed known issue 8: AniList search drops `format: NOVEL` entries; `--id anilist:N` still reaches a novel.
- Verified the cross-filesystem move (former "not verified" item 4), see below.

### Verified

- `cargo test --workspace` passes: 36 unit tests, the corpus (76 cases), 3 pipeline integration tests and 6 CLI tests.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- Manual offline run on dummy files: dry run, real move with `--clean`, `history`, and `undo`. Undo restored everything, including folders pruned by `--clean`, and removed the created library folders.
- **Live AniList:** `[SubsPlease] Sousou no Frieren - 12` → anilist:154587; One Piece and Chainsaw Man manga matched at 100%.
- **Live extras:** the manga cover downloaded as a real PNG, ComicInfo.xml was embedded in a `.cbz` (checked with `unzip`), and undo removed the cover.
- Without a TMDB key, movies/TV fail with a clear message and exit code 1.
- **Cross-filesystem move** (2026-11-01): release binary, NTFS fuseblk `/mnt/shared` → tmpfs library with
  `--clean`, then `mrn undo`. Video and sidecar moved and came back with identical SHA-256; undo removed the created library folders.
- **AppImage** (2026-11-01): `packaging/appimage/build.sh` produced `dist/mrn-0.1.0-x86_64.AppImage` (6.1 MB,
  appimagetool continuous build 340, downloaded by the script). Installed as `mrn` on a temp `PATH`, it ran an offline
  rename into a library and `undo` restored it; `--version` also works with `APPIMAGE_EXTRACT_AND_RUN=1`.

### Not verified yet

1. **TMDB against the real API** (movies, TV, and anime episode titles). No key was available, so only wiremock tests cover it. Response shapes follow the TMDB v3 docs.
2. **ComicVine against the real API** (needs a key). Same situation.
3. **The interactive picker** (`ui.rs::prompt`) in a real terminal. Only the `Chooser` logic is tested, with a scripted chooser.
4. **`build.sh --container`** (Debian bookworm build for glibc 2.36) was never run, and the AppImage
   hasn't been tried on another distro.
5. `hardlink` and `symlink` were unit-tested in a tempdir only, never through the CLI on real media.

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
- **Git:** the first commit is tagged `v0.1.0` (2026-11-01, branch `master`). No remote is configured.

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
| 2 | Undo doesn't remove ComicInfo.xml from a cbz it moves back | `history.rs` | could back up the original or record a "modify" op |
| 3 | A file replaced with `--conflict override` can't be restored | `ops.rs`, `history.rs` | could move it to a trash folder first |
| 4 | Chain renames in one batch (A→B while B→C) are treated as conflicts | `ops::build_plan` | the on-disk check doesn't know B will move away |
| 5 | Files of one show with and without a year in the name form two lookup groups (two prompts) | `pipeline.rs` step 2 | could merge groups by title and resolve the year after |
| 6 | Anime episode titles depend on a TMDB title cross-reference. Later-season AniList entries whose numbering restarts usually get no titles, which is deliberate (0.85 threshold plus year penalty) | `Engine::series_for` | possible improvement: AniList→TMDB id mapping via a mapping database |
| 7 | ComicVine allows 200 requests per hour, so big comic batches with `{t}`/`--comicinfo` are slow | `providers/http.rs` | the cache helps on reruns |
| 8 | ~~AniList manga search can return light novels~~ | `providers/anilist.rs` | fixed 2026-11-01: novels are filtered from search; light-novel `.epub` files need `--id anilist:N` |
| 9 | ComicInfo is only embedded in `.cbz`/`.zip`, not `.cbr`, `.cb7`, `.pdf` or `.epub` | `extras/comicinfo.rs` | RAR can't be written |
| 10 | Parser heuristics have edge cases: a bare `Episode N` without a season is treated as anime; `S2` in a title is read as a season; the book `c`/`v` prefixes can misfire | `parse/` | grow the corpus with real names |
| 11 | Spinner and progress bar are hidden without a TTY; applying changes non-interactively requires `--yes` | `mrn-cli/src/ui.rs`, `rename.rs` | deliberate |
| 12 | `--clean` only removes folders that end up completely empty (leftover `.txt`/samples keep them) | `ops::prune_empty_dirs` | deliberate, safe |

## Roadmap (suggested order)

**Next (finish v1):**
1. Verify TMDB live with a real key: movies, TV, daily shows, and anime cross-reference. Fix any response-shape surprises.
2. Verify ComicVine live (key). Run `packaging/appimage/build.sh --container` and try the result on an older distro.
3. Grow the corpus toward 150+ using real names from your library.
4. Commit, tag `v0.1.0`, attach the AppImage to a release, write a PKGBUILD (Arch/CachyOS).

**Then:**
- `--json` output for `rename -n`, `info` and `history` (scripting, GUI prototyping).
- Undo for overwritten files (issue 3) and ComicInfo (issue 2).
- Episode ordering options (DVD/absolute) and a per-run title language.
- MangaDex chapter titles for `{t}` on manga.

**Later (v2):**
- **GUI.** Build it on `mrn-core::pipeline` with a dialog-based `Chooser`. PyGObject was considered earlier; in Rust, gtk4-rs/relm4 fits a GTK desktop. Detect the desktop's toolkit first.
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
| AppImage as the binary package; desktop entry with `Terminal=true` | User asked for it; mrn is a CLI, so the entry only matters for AppImage integrators |
| AppImage built with `APPIMAGE_EXTRACT_AND_RUN=1` and `--no-appstream` | Works without FUSE in containers/CI; no AppStream id is settled yet |
| `mrn.png` committed next to `mrn.svg` | appimagetool rejects an SVG `.DirIcon`; a committed PNG keeps the build free of rsvg/ImageMagick |
| ComicInfo timestamp computed by hand | The zip `time` feature would pull in the `time` crate for one call |
