#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
version="$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' Cargo.toml | head -1)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Invalid workspace version' >&2; exit 1; }
[[ "$(uname -m)" == x86_64 ]] || { echo 'Only x86_64 packaging is supported' >&2; exit 1; }

tool="$root/target/appimage-tools/appimagetool-x86_64.AppImage"
tool_sha='ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0'
mkdir -p "$(dirname "$tool")"
if [[ ! -f "$tool" ]]; then
    curl --fail --location --retry 3 --max-time 120 \
        'https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage' \
        -o "$tool.part"
    mv "$tool.part" "$tool"
fi
printf '%s  %s\n' "$tool_sha" "$tool" | sha256sum --check --status || { echo 'appimagetool digest mismatch' >&2; exit 1; }
chmod +x "$tool"
runtime="$root/target/appimage-tools/runtime-x86_64"
runtime_sha='617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46'
if [[ ! -f "$runtime" ]]; then
    curl --fail --location --retry 3 --max-time 120 \
        'https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64' \
        -o "$runtime.part"
    mv "$runtime.part" "$runtime"
fi
printf '%s  %s\n' "$runtime_sha" "$runtime" | sha256sum --check --status || { echo 'AppImage runtime digest mismatch' >&2; exit 1; }

appdir="$root/target/AppDepot.AppDir"
rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/lib" "$appdir/usr/share/applications" "$appdir/usr/share/metainfo" "$root/dist"
install -m755 target/release/app-depot-desktop "$appdir/usr/bin/app-depot-desktop"
install -m755 target/release/app-depot-update-helper "$appdir/usr/bin/app-depot-update-helper"
install -m755 assets/AppRun "$appdir/AppRun"
install -m644 assets/io.github.viktorliho.AppDepot.desktop "$appdir/io.github.viktorliho.AppDepot.desktop"
printf 'X-AppImage-Version=%s\n' "$version" >> "$appdir/io.github.viktorliho.AppDepot.desktop"
install -m644 "$appdir/io.github.viktorliho.AppDepot.desktop" "$appdir/usr/share/applications/io.github.viktorliho.AppDepot.desktop"
install -m644 assets/io.github.viktorliho.AppDepot.appdata.xml "$appdir/usr/share/metainfo/io.github.viktorliho.AppDepot.appdata.xml"
install -m644 assets/io.github.viktorliho.AppDepot-256.png "$appdir/io.github.viktorliho.AppDepot.png"
for size in 256 128 64 48 32; do
    icon_dir="$appdir/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$icon_dir"
    install -m644 "assets/io.github.viktorliho.AppDepot-${size}.png" "$icon_dir/io.github.viktorliho.AppDepot.png"
done
ln -s io.github.viktorliho.AppDepot.png "$appdir/.DirIcon"

# Bundle linked user-space libraries from the Ubuntu release baseline. Fundamental
# glibc/runtime and graphics drivers remain host-provided to avoid ABI conflicts.
for binary in "$appdir/usr/bin/app-depot-desktop" "$appdir/usr/bin/app-depot-update-helper"; do
    while read -r soname _ path rest; do
        [[ "${path:-}" == /* ]] || continue
        case "$soname" in
            libc.so.*|libm.so.*|libpthread.so.*|libdl.so.*|librt.so.*|libgcc_s.so.*|ld-linux*|*/ld-linux*|libGL*|libEGL*|libdrm*|libwayland*|libX11*|libxcb*|libxkbcommon*) continue ;;
        esac
        install -m644 "$path" "$appdir/usr/lib/$soname"
    done < <(ldd "$binary")
done
if ldd "$appdir/usr/bin/app-depot-desktop" | grep -q 'not found'; then
    echo 'Unresolved desktop dependencies' >&2
    exit 1
fi

work="$root/target/appimage-tools/extract"
rm -rf "$work"
mkdir -p "$work"
(cd "$work" && "$tool" --appimage-extract >/dev/null)
image="$root/dist/AppDepot-${version}-x86_64.AppImage"
rm -f "$image" "$image.zsync"
SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)" \
ARCH=x86_64 "$work/squashfs-root/AppRun" \
    --runtime-file "$runtime" \
    -u 'gh-releases-zsync|viktorliho|app-depot-client|latest|AppDepot-*-x86_64.AppImage.zsync' \
    "$appdir" "$image"
chmod +x "$image"
test "$(dd if="$image" bs=1 skip=8 count=3 status=none)" = $'AI\002'
if [[ -f "$root/$(basename "$image").zsync" ]]; then
    mv "$root/$(basename "$image").zsync" "$image.zsync"
fi
test -s "$image.zsync"
(cd "$root/dist" && sha256sum "$(basename "$image")" > "$(basename "$image").sha256")
printf 'Built %s\n' "$image"
