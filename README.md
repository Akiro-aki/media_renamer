# mrn: media renamer

A FileBot-style renamer for Linux. It reads messy file names, matches them
against online databases, and renames or moves them into a clean library
layout for Jellyfin, Plex, Kodi, Kavita or Komga.

| Media  | Matched with | API key |
|--------|--------------|---------|
| Movies, TV | [TMDB](https://www.themoviedb.org/settings/api) | free key needed |
| Anime  | AniList (episode titles via TMDB when a key is set) | none |
| Manga  | AniList, MangaDex as fallback | none |
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

Put your keys in the config file, or set `TMDB_API_KEY` and `COMICVINE_API_KEY`.

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
| `-q TITLE` / `--id tmdb:1399` | search a different title, or use a specific entry (`tmdb`, `anilist`, `mangadex`, `comicvine`) |
| `-f PRESET\|TEMPLATE` | `jellyfin` (default), `plex`, `kodi`, `anime`, `kavita`, `komga`, or a template |
| `-a ACTION` | `move` (default), `copy`, `hardlink`, `symlink` |
| `--conflict` | `skip` (default), `override`, `index` (`Name (1).mkv`) |
| `--nfo` `--artwork` `--comicinfo` | opt-in extras (see below) |
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
  `{publisher}`, `{orig}` original file name.
- `{s:02}` zero-pads; `{n:upper}`, `{n:lower}`, `{n:first}` (first letter, for A–Z folders).
- `[ … ]` is optional: it's dropped when a field inside it has no value.
- `\[` `\]` `\{` `\}` are literal brackets. `/` separates folders. The extension is added for you.

Set per-kind templates under `[format]` in the config.

## Extras (off by default)

- `--nfo`: Kodi/Jellyfin `movie.nfo`/`tvshow.nfo`/episode `.nfo`.
- `--artwork`: posters, fanart, season posters, comic/manga `cover.jpg`.
- `--comicinfo`: ComicInfo.xml embedded into `.cbz` (`.cbr` can't be written).

Series-level files (`tvshow.nfo`, `poster.jpg`, `cover.jpg`) are only written
inside a library root (`-o` or `[output]`), never into your downloads folder.
Existing files are never overwritten. To turn extras on permanently, set them
under `[extras]` in the config; `--no-extras` turns them off for one run.

## Undo

Every operation is logged to `~/.local/share/mrn/history.db` before it runs.
`mrn undo [BATCH]` moves files back, deletes created links, copies and extras,
and removes folders the batch created. Files changed since the rename are left
alone unless you pass `--force`. Limits: a file replaced with
`--conflict override` can't be restored, and ComicInfo.xml stays inside a cbz
that is moved back.

Moves across filesystems (e.g. from an NTFS or network mount into your home)
fall back to copy-and-delete, and undo works the same way.

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
