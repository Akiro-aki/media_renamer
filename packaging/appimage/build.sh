#!/usr/bin/env bash
# Build dist/mrn-<version>-<arch>.AppImage.
#
#   packaging/appimage/build.sh              build with the host toolchain
#   packaging/appimage/build.sh --container  build the binary in a Debian container
#                                            (older glibc, so the AppImage runs on more distros)
#
# Environment:
#   APPIMAGETOOL      path to appimagetool (default: on PATH, else downloaded to the cache)
#   MRN_BUILD_IMAGE   container image for --container (default: docker.io/library/rust:1-bookworm)
#   CONTAINER_ENGINE  podman or docker (default: whichever is installed, podman first)
#   CARGO_TARGET_DIR  respected for host builds
set -euo pipefail

usage() { sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; }

container=0
for arg in "$@"; do
    case "$arg" in
        --container) container=1 ;;
        -h | --help) usage; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; usage >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
arch="$(uname -m)"
cache="${XDG_CACHE_HOME:-$HOME/.cache}/mrn-build"
mkdir -p "$cache"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

# 1. The binary.
if ((container)); then
    engine="${CONTAINER_ENGINE:-$(command -v podman || command -v docker || true)}"
    [[ -n "$engine" ]] || { echo "--container needs podman or docker" >&2; exit 1; }
    image="${MRN_BUILD_IMAGE:-docker.io/library/rust:1-bookworm}"
    target="$cache/container-target"
    mkdir -p "$target" "$cache/cargo-registry"
    log "Building in $image with $(basename "$engine")"
    # :z relabels for SELinux hosts; it is ignored elsewhere.
    "$engine" run --rm \
        -v "$root:/src:z" -v "$target:/target:z" -v "$cache/cargo-registry:/usr/local/cargo/registry:z" \
        -e CARGO_TARGET_DIR=/target -w /src \
        "$image" cargo build --release --locked -p mrn-cli -p mrn-gui
    bin="$target/release/mrn"
else
    log "Building with $(cargo --version)"
    (cd "$root" && cargo build --release --locked -p mrn-cli -p mrn-gui)
    target_dir="$(cd "$root" && cargo metadata --format-version 1 --no-deps |
        sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
    bin="$target_dir/release/mrn"
fi
[[ -x "$bin" ]] || { echo "binary not found: $bin" >&2; exit 1; }

version="$("$bin" --version | awk '{print $2}')"
glibc="$(objdump -T "$bin" "$(dirname "$bin")/mrn-gui" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1 || true)"

# 2. The AppDir.
appdir="$cache/AppDir"
log "Assembling AppDir for mrn $version"
rm -rf "$appdir"
install -Dm755 "$bin" "$appdir/usr/bin/mrn"
install -Dm755 "$(dirname "$bin")/mrn-gui" "$appdir/usr/bin/mrn-gui"
install -Dm755 "$here/AppRun" "$appdir/AppRun"
install -Dm644 "$here/mrn.desktop" "$appdir/mrn.desktop"
install -Dm644 "$here/mrn.desktop" "$appdir/usr/share/applications/mrn.desktop"
# appimagetool wants a PNG at the top (it becomes .DirIcon). mrn.png is
# rendered from mrn.svg: rsvg-convert -w 256 -h 256 mrn.svg -o mrn.png
install -Dm644 "$here/mrn.png" "$appdir/mrn.png"
install -Dm644 "$here/mrn.png" "$appdir/usr/share/icons/hicolor/256x256/apps/mrn.png"
install -Dm644 "$here/mrn.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/mrn.svg"
"$bin" man | gzip -9n | install -Dm644 /dev/stdin "$appdir/usr/share/man/man1/mrn.1.gz"
"$bin" completions bash | install -Dm644 /dev/stdin "$appdir/usr/share/bash-completion/completions/mrn"
"$bin" completions zsh | install -Dm644 /dev/stdin "$appdir/usr/share/zsh/site-functions/_mrn"
"$bin" completions fish | install -Dm644 /dev/stdin "$appdir/usr/share/fish/vendor_completions.d/mrn.fish"
for f in README.md LICENSE-MIT LICENSE-APACHE; do
    [[ -f "$root/$f" ]] && install -Dm644 "$root/$f" "$appdir/usr/share/doc/mrn/$f"
done

# 3. appimagetool.
tool="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
if [[ -z "$tool" ]]; then
    tool="$cache/appimagetool-$arch.AppImage"
    if [[ ! -x "$tool" ]]; then
        url="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage"
        log "Downloading appimagetool"
        curl -fL --retry 3 -o "$tool.part" "$url"
        chmod +x "$tool.part"
        mv "$tool.part" "$tool"
    fi
fi

# 4. The AppImage. Extract-and-run so the build works without FUSE (containers, CI).
out="$root/dist/mrn-$version-$arch.AppImage"
mkdir -p "$root/dist"
rm -f "$out"
log "Packing $out"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" VERSION="$version" "$tool" --no-appstream "$appdir" "$out"

"$out" --version >/dev/null
log "Done: $out ($(du -h "$out" | cut -f1), needs ${glibc:-glibc} or newer)"
