#!/usr/bin/env bash
set -euo pipefail
umask 022

# Build a self-contained AppImage from the same payload as package.sh.
# Arguments are accepted for pipeline symmetry (archive|deb|rpm|all) and ignored.
#
# Requires: mksquashfs (squashfs-tools), curl, file. The Type 2 runtime is
# fetched at build time from the AppImage project; nothing is downloaded at
# run time and the finished image needs no FUSE to start.

mectov_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$mectov_root"

case ${1:-archive} in
    archive|deb|rpm|all) ;;
    *) printf 'Usage: %s [archive|deb|rpm|all]\n' "$0" >&2; exit 1 ;;
esac

for mectov_tool in mksquashfs curl file; do
    command -v "$mectov_tool" >/dev/null || { printf 'Required tool: %s\n' "$mectov_tool" >&2; exit 1; }
done

mectov_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
mectov_name="mectov-${mectov_version}-linux-$(uname -m)"

# The AppImage ships byte-identical runtime files to the other formats.
if [[ ! -f "dist/$mectov_name.tar.gz" ]]; then
    scripts/package.sh archive
fi

mectov_temporary=$(mktemp -d "$mectov_root/dist/.appimage.XXXXXX")
trap 'rm -rf -- "$mectov_temporary"' EXIT
mectov_appdir="$mectov_temporary/$mectov_name.AppDir"
mkdir -p "$mectov_appdir/usr"
tar -xzf "dist/$mectov_name.tar.gz" -C "$mectov_temporary"
mv "$mectov_temporary/$mectov_name"/* "$mectov_appdir/usr/"

install -m755 packaging/AppRun "$mectov_appdir/AppRun"
install -Dm644 packaging/me.silverl.mectov.desktop "$mectov_appdir/me.silverl.mectov.desktop"
install -m644 "assets/icons/hicolor/256x256/apps/me.silverl.mectov.png" "$mectov_appdir/me.silverl.mectov.png"

# Keep the image byte-reproducible: fixed filesystem timestamps and traversal order.
mksquashfs "$mectov_appdir" "$mectov_temporary/squashfs.img" \
    -noappend -root-mode 0755 -mkfs-time 0 -all-time 0 \
    -no-fragments -no-xattrs -quiet -no-progress

mectov_runtime_image="$mectov_temporary/runtime-x86_64"
curl --fail --silent --show-error --location --output "$mectov_runtime_image" \
    'https://github.com/AppImage/type2-runtime/releases/latest/download/runtime-x86_64'
file --brief "$mectov_runtime_image" | grep -q 'ELF'
printf 'AppImage type2 runtime sha256: %s\n' "$(sha256sum "$mectov_runtime_image" | cut -d' ' -f1)"

cat "$mectov_runtime_image" "$mectov_temporary/squashfs.img" > "dist/$mectov_name.AppImage"
chmod 755 "dist/$mectov_name.AppImage"
(cd dist && sha256sum "$mectov_name.AppImage" > "$mectov_name.AppImage.sha256")
printf 'Created %s/dist/%s.AppImage\n' "$mectov_root" "$mectov_name"
