#!/usr/bin/env bash
# v7.8 tickets 61–62: build an x86-64 AppImage of the committed HEAD into target/appimage/.
# Usage: scripts/appimage.sh [label] [--no-arch]   (label defaults to "local"; e.g. preview.1)
# Only `git archive HEAD` is built, so uncommitted edits never ship. appimagetool and the
# runtime are pinned and checked by SHA-256. Publishing to GitHub is a separate step.
# After the build, a disposable Arch Linux container runs the AppImage's --version and --help
# (no controller, keys or desktop): skipped with --no-arch or without podman, and said so.
set -euo pipefail
cd "$(dirname "$0")/.."

label=local arch=1
for a in "$@"; do
    case "$a" in
        --no-arch) arch=0 ;;
        -*) echo "appimage (arguments): unknown option $a" >&2; exit 2 ;;
        *) label=$a ;;
    esac
done
# The label lands in a file name and the build string: keep it a plain identifier.
[[ $label =~ ^[A-Za-z0-9._-]{1,40}$ ]] || { echo "appimage (arguments): label must be letters, digits, . _ - (got '$label')" >&2; exit 2; }

out=target/appimage
tools=$out/tools
mkdir -p "$tools"

# Pinned tools (checked 25 Nov 2026 against the GitHub releases named in the URLs).
fetch() { # name url sha256
    local f=$tools/$1
    if [ ! -f "$f" ] || ! echo "$3  $f" | sha256sum -c --quiet - >/dev/null 2>&1; then
        curl -fsSL -o "$f.part" "$2" || { echo "appimage (tools): download of $1 failed: $2" >&2; exit 1; }
        if ! echo "$3  $f.part" | sha256sum -c --quiet - >/dev/null 2>&1; then
            rm -f "$f.part"
            echo "appimage (tools): $1 checksum mismatch; expected $3 from $2" >&2; exit 1
        fi
        mv "$f.part" "$f"
    fi
    chmod +x "$f"
}
fetch appimagetool https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
    ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
fetch runtime https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
    2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

# Source: the committed HEAD only.
commit=$(git rev-parse --short HEAD)
version=$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -1)
[ "$label" = local ] && name_version="$version-$commit" || name_version="$version-$label"
git diff --quiet HEAD || echo "appimage: uncommitted edits are left out; building commit $commit"
src=$out/src
rm -rf "$src" && mkdir -p "$src"
git archive HEAD | tar -x -C "$src"

echo "appimage (build): commit $commit, label $label"
VIBECONSOLE_BUILD="$label, commit $commit" CARGO_TARGET_DIR=$PWD/$out/build \
    cargo build --manifest-path "$src/Cargo.toml" --release --locked \
    || { echo "appimage (build): cargo build failed" >&2; exit 1; }

# AppDir from packaging/appimage (as committed, i.e. from $src).
dir=$out/VibeConsole.AppDir
rm -rf "$dir" && mkdir -p "$dir/usr/bin"
install -m 755 "$out/build/release/vibeconsole" "$dir/usr/bin/vibeconsole"
install -m 755 "$src/packaging/appimage/AppRun" "$dir/AppRun"
install -m 644 "$src/packaging/appimage/vibeconsole."{desktop,png,svg} "$dir/"
ln -s vibeconsole.png "$dir/.DirIcon"

img=VibeConsole-$name_version-x86_64.AppImage
ARCH=x86_64 VERSION=$name_version APPIMAGE_EXTRACT_AND_RUN=1 \
    "$tools/appimagetool" --no-appstream --runtime-file "$tools/runtime" "$dir" "$out/$img.part" \
    || { echo "appimage (pack): appimagetool failed" >&2; rm -f "$out/$img.part"; exit 1; }
mv "$out/$img.part" "$out/$img"
(cd "$out" && sha256sum "$img" > "$img.sha256.part" && mv "$img.sha256.part" "$img.sha256")
APPIMAGE_EXTRACT_AND_RUN=1 "$out/$img" --version
glibc=$(readelf --version-info "$dir/usr/bin/vibeconsole" | grep -oE 'GLIBC_[0-9.]+' | sort -Vu | tail -1)
echo "appimage: $out/$img (needs ${glibc/_/ } or later)"

# Arch check (ticket 62).
if [ "$arch" = 0 ]; then echo "appimage (arch): skipped (--no-arch)"; exit 0; fi
if ! command -v podman >/dev/null; then echo "appimage (arch): skipped (podman not installed)"; exit 0; fi
podman run --rm --security-opt label=disable -v "$PWD/$out:/release:ro" -e "IMG=$img" \
    docker.io/library/archlinux:latest bash -ec '
        pacman -Sy --noconfirm --needed --quiet alsa-lib systemd-libs >/dev/null
        echo "arch: $(getconf GNU_LIBC_VERSION)"
        cd /release && sha256sum -c "$IMG.sha256"
        APPIMAGE_EXTRACT_AND_RUN=1 "./$IMG" --version
        APPIMAGE_EXTRACT_AND_RUN=1 "./$IMG" --help >/dev/null && echo "arch: --help ok"' \
    || { echo "appimage (arch): the AppImage failed in the Arch container (see above)" >&2; exit 1; }
echo "appimage (arch): passed"
