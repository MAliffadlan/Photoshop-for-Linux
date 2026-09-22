#!/usr/bin/env bash
set -euo pipefail

mectov_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mectov_prefix=${1:-"$HOME/.local"}
mectov_binary="$mectov_root/bin/mectov"
if [[ ! -f "$mectov_binary" ]]; then
    mectov_binary="$mectov_root/target/release/mectov"
fi
if [[ ! -f "$mectov_binary" ]]; then
    printf 'Build first with cargo build --release, or extract a release archive.\n' >&2
    exit 1
fi
install -Dm755 "$mectov_binary" "$mectov_prefix/bin/mectov"
if [[ -d "$mectov_root/share" ]]; then
    install -d "$mectov_prefix/share"
    cp -R --preserve=mode "$mectov_root/share/." "$mectov_prefix/share/"
else
    for mectov_icon in "$mectov_root"/assets/icons/hicolor/*/apps/me.silverl.mectov.png; do
        install -Dm644 "$mectov_icon" "$mectov_prefix/share/icons/${mectov_icon#"$mectov_root/assets/icons/"}"
    done
    install -Dm644 "$mectov_root/packaging/me.silverl.mectov.desktop" "$mectov_prefix/share/applications/me.silverl.mectov.desktop"
    install -Dm644 "$mectov_root/packaging/me.silverl.mectov.xml" "$mectov_prefix/share/mime/packages/me.silverl.mectov.xml"
    install -Dm644 "$mectov_root/LICENSE" "$mectov_prefix/share/licenses/mectov/LICENSE"
    install -Dm644 "$mectov_root/THIRD_PARTY.md" "$mectov_prefix/share/licenses/mectov/THIRD_PARTY.md"
    install -Dm644 "$mectov_root/licenses/rawler-LGPL-2.1.txt" "$mectov_prefix/share/licenses/mectov/rawler-LGPL-2.1.txt"
    install -Dm644 "$mectov_root/assets/fonts/Inter-LICENSE.txt" "$mectov_prefix/share/licenses/mectov/Inter-LICENSE.txt"
    for mectov_license in LICENSE-MIT LICENSE-APACHE; do
        install -Dm644 "$mectov_root/vendor/egui-winit/$mectov_license" "$mectov_prefix/share/licenses/mectov/egui-winit/$mectov_license"
    done
fi
# Remove the previous logo so desktops cannot select it as a scalable fallback.
rm -f -- "$mectov_prefix/share/icons/hicolor/scalable/apps/me.silverl.xuan.svg"
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -f -t "$mectov_prefix/share/icons/hicolor"
fi
if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$mectov_prefix/share/applications"
fi
if command -v update-mime-database >/dev/null; then
    update-mime-database "$mectov_prefix/share/mime"
fi
printf 'Installed mectov to %s. Ensure %s/bin is in PATH.\n' "$mectov_prefix" "$mectov_prefix"
