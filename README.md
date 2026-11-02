# mrn: media renamer

A FileBot-style renamer for Linux. It reads messy file names, matches them
against online databases, and renames or moves them into a clean library
layout for Jellyfin, Plex, Kodi, Kavita or Komga.

| Media  | Matched with | API key |
|--------|--------------|---------|
| Movies, TV | [TMDB](https://www.themoviedb.org/settings/api) | free key needed |
| Anime  | AniList; episode titles from [TheTVDB](https://thetvdb.com/api-information) and/or TMDB when their keys are set (MyAnimeList can be added, see [Providers](#providers)) | none to match; a TheTVDB or TMDB key for episode titles |
| Manga  | AniList, then [MangaBaka](https://mangabaka.org), then MangaDex; volume covers from MangaDex | none |
| Comics | [ComicVine](https://comicvine.gamespot.com/api/) | free key; without one, files are named from the file name |

## Graphical interface

`mrn-gui` (or `mrn gui`) does the same as `mrn rename`, in a window:

1. **Add files…** or **Add folder…**, and optionally a **Library folder** to move them into.
2. Pick the media type, action, naming preset and extras, then **Preview**.
3. If a match is uncertain, pick the right title from the list, search with another title, or skip.
4. Check the new names and press **Move N files**. **Undo** reverts the batch right away; the
   **History** tab lists every batch, from the CLI too, with an Undo button each.

The **Settings** tab stores your TMDB and ComicVine keys in the config file and opens the file
for everything else. The AppImage opens the GUI when you start it from your app menu or file
manager, and behaves as the `mrn` CLI when you run it in a terminal.

## Install

### AppImage (any x86_64 distro)

One self-contained file with the CLI and the GUI. It needs glibc 2.35 or newer
(Ubuntu 22.04+, Debian 12+, Fedora 36+, Arch) and, for the GUI, a Vulkan or OpenGL driver.
Download it from the [releases page](https://github.com/Akiro-aki/media_renamer/releases).

```sh
install -Dm755 mrn-0.2.0-x86_64.AppImage ~/.local/bin/mrn
mrn config init          # writes ~/.config/mrn/config.toml
mrn gui                  # or double-click the AppImage
```

`~/.local/bin` must be on your `PATH`. To get a menu entry, use AppImageLauncher
or Gear Lever, or copy `mrn.desktop` from `packaging/appimage/` to
`~/.local/share/applications/` and change its `Exec=` to the AppImage's path. Shell completions and a man page:

```sh
mrn completions fish > ~/.config/fish/completions/mrn.fish
mrn completions bash > ~/.local/share/bash-completion/completions/mrn
mrn completions zsh  > ~/.zfunc/_mrn          # a directory in your $fpath
mrn man | man -l -
```

If FUSE isn't available (some containers), run it with
`--appimage-extract-and-run` or set `APPIMAGE_EXTRACT_AND_RUN=1`.

Build the AppImage yourself (output goes to `dist/`):

```sh
packaging/appimage/build.sh               # host toolchain
packaging/appimage/build.sh --container   # in Debian bookworm (podman/docker): runs on most distros
```

The script uses `appimagetool` from `PATH` (or `APPIMAGETOOL=/path`) and
otherwise downloads it from the AppImage project's GitHub releases. A host
build needs the same or newer glibc on the target as the build machine
(the script prints the minimum), which on a rolling distro is very recent;
use `--container` for releases (it measured glibc 2.35: Ubuntu 22.04+, Debian 12+, Fedora 36+).

### From source

```sh
cargo install --path crates/mrn-cli
cargo install --path crates/mrn-gui      # optional, the GUI
mrn config init
```

The GUI needs a Vulkan or OpenGL driver and Wayland or X11 at runtime; nothing extra at build time.

Put your keys in the config file (or the GUI's Settings tab), or set `TMDB_API_KEY`,
`COMICVINE_API_KEY`, `TVDB_API_KEY` and `TVDB_PIN`. TheTVDB keys come from your account
dashboard on thetvdb.com; the PIN is only needed for keys that ask for a subscriber PIN.

## Usage

```sh
mrn rename ~/Downloads -o ~/Media -n     # preview (dry run)
mrn rename ~/Downloads -o ~/Media        # preview, confirm, apply
mrn rename show.s01e02.mkv               # rename in place, file name only
mrn info "[SubsPlease] Frieren - 12 (1080p).mkv"   # what mrn reads and matches
mrn history                              # past batches
mrn undo                                 # revert the last batch
```

When a match is uncertain (below `threshold`, 0.85 by default), mrn shows the
best candidates and lets you pick one, search with another title, or skip.
With `--non-interactive` (or with no terminal), uncertain files are skipped.

Useful flags for `rename`:

| Flag | Meaning |
|------|---------|
| `-o DIR` | library root; without it files are renamed in place |
| `-t TYPE` | force `movie`, `tv`, `anime`, `comic` or `manga` |
| `-q TITLE` / `--id tmdb:1399` | search a different title, or use a specific entry (`tmdb`, `anilist`, `mal`, `mangabaka`, `mangadex`, `comicvine`) |
| `--providers LIST=SOURCES` | search order for this run, e.g. `manga=mangabaka,anilist` (lists: `anime`, `manga`, `anime_episodes`) |
| `-f PRESET\|TEMPLATE` | `jellyfin` (default), `plex`, `kodi`, `anime`, `kavita`, `komga`, or a template |
| `-a ACTION` | `move` (default), `copy`, `hardlink`, `symlink` |
| `--conflict` | `skip` (default), `override` (the replaced file goes to the trash), `index` (`Name (1).mkv`) |
| `--nfo` `--artwork` `--comicinfo` `--embed-cover` | opt-in extras (see below) |
| `--offline` | no lookups; tidy names from the file name only |
| `--clean` | remove source folders left empty |
| `-n` / `-y` | dry run / don't ask for confirmation |

Subtitles and other sidecar files (`Name.en.srt`, `Name.nfo`, …) move with their video.

## Templates

```
{n} ({y})/Season {s:02}/{n} - {s00e00}[ - {t}]
```

- `{n}` name, `{y}` year, `{s}` season, `{e}` episode, `{s00e00}`, `{sxe}`, `{abs}` absolute episode,
  `{t}` episode/issue title, `{d}` air date, `{vol}`, `{ch}`, `{issue}`, `{vf}` resolution,
  `{vs}` source, `{vc}`/`{ac}` codecs, `{group}`, `{crc}`, `{id}`, `{tmdbid}`, `{anilistid}`,
  `{malid}`, `{mangabakaid}` (also filled from cross-site ids, e.g. `{malid}` for an AniList match),
  `{source}`, `{publisher}`, `{orig}` original file name.
- `{s:02}` zero-pads; `{n:upper}`, `{n:lower}`, `{n:first}` (first letter, for A–Z folders).
- `[ … ]` is optional: it's dropped when a field inside it has no value.
- `\[` `\]` `\{` `\}` are literal brackets. `/` separates folders. The extension is added for you.

Set per-kind templates under `[format]` in the config.

## Providers

Anime and manga are searched in the order set under `[providers]` in the config;
the first confident match wins. `--providers` overrides it for one run, and the
GUI's Settings tab shows the current order.

```toml
[providers]
anime = ["anilist"]
manga = ["anilist", "mangabaka", "mangadex"]
# Where anime episode titles ({t}) come from: each source fills the gaps of the one before.
anime_episodes = ["tvdb", "tmdb"]
```

TheTVDB numbers anime in the seasons Sonarr, Plex and Jellyfin expect, so it
comes first; sources without a key are skipped. An anime is found on TheTVDB
and TMDB by its English or romaji title.

MyAnimeList, through the unofficial [Jikan](https://jikan.moe) API, is available
but not in the defaults: add `"mal"` to `anime` (search fallback) or
`anime_episodes` (episode titles without a TMDB key). With it, later seasons
that AniList and MyAnimeList list as separate entries are found through
AniList's sequel links, so `Show S02E03` or `Show - 31` gets the right title.

A host that can't be reached is skipped for 5 minutes, so the next source
answers instead of waiting.

## Extras (off by default)

- `--nfo`: Kodi/Jellyfin `movie.nfo`/`tvshow.nfo`/episode `.nfo`.
- `--artwork`: posters, fanart, season posters, comic/manga `cover.jpg`.
- `--comicinfo`: ComicInfo.xml embedded into `.cbz` (`.cbr` can't be written), with the series'
  genres, tags, writers and artists, total count (finished series) and age rating for Kavita and Komga.
- Volume covers (manga from MangaDex, comic issues from ComicVine):
  - **Kavita:** `--embed-cover` puts the cover inside each `.cbz` as its first page (`!cover.jpg`),
    unless the archive already has a cover image. Kavita only reads covers from inside the archive.
  - **Komga:** `--artwork -f komga` saves it beside each book (`Vol 01.jpg` next to `Vol 01.cbz`),
    which Komga prefers; the archive is not touched. Don't do this for Kavita: it would list the
    image as an extra chapter.

Series-level files (`tvshow.nfo`, `poster.jpg`, `cover.jpg`) are only written
inside a library root (`-o` or `[output]`), never into your downloads folder.
Existing files are never overwritten. To turn extras on permanently, set them
under `[extras]` in the config; `--no-extras` turns them off for one run.

## Undo

Every operation is logged to `~/.local/share/mrn/history.db` before it runs.
`mrn undo [BATCH]` moves files back, deletes created links, copies and extras,
removes ComicInfo.xml it embedded (restoring an older one), and removes folders
the batch created. Files changed since the rename are left alone unless you
pass `--force`.

A file replaced with `--conflict override` is moved to the trash (the one on
the same disk, so nothing is copied), where your file manager can also restore
it. `mrn undo` puts it back.

Renames that depend on each other in one batch (A→B while B→C) run in the right
order; two files that would trade names are skipped.

Moves across filesystems (e.g. from an NTFS or network mount into your home)
fall back to copy-and-delete, and undo works the same way.

## Credits

Metadata provided by [TheTVDB](https://thetvdb.com). Please consider adding missing
information or subscribing. Also [TMDB](https://www.themoviedb.org) (this product uses the
TMDB API but is not endorsed or certified by TMDB), [AniList](https://anilist.co),
[MangaBaka](https://mangabaka.org), [MangaDex](https://mangadex.org),
[ComicVine](https://comicvine.gamespot.com) and MyAnimeList via [Jikan](https://jikan.moe).

## Development

- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): status, known issues, roadmap, how to resume.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it works inside.

## Layout

- `crates/mrn-core`: library (parse → detect → providers/matcher → format → ops/history, extras).
  The `pipeline` module is the API a GUI would use.
- `crates/mrn-cli`: the `mrn` binary.
- `crates/mrn-gui`: the `mrn-gui` binary (iced).
- `packaging/appimage`: AppImage build script, `AppRun`, desktop entry, icon.
- `crates/mrn-core/tests/fixtures/filenames.toml`: parser corpus. Add a case
  when you find a file name that parses wrong.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
