#!/usr/bin/env bash
set -euo pipefail

mectov_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if ! command -v magick >/dev/null; then
    printf 'Generating icons requires ImageMagick 7 (magick).\n' >&2
    exit 1
fi

for mectov_size in 16 24 32 48 64 96 128 256 512 1024; do
    mectov_directory="$mectov_root/assets/icons/hicolor/${mectov_size}x${mectov_size}/apps"
    mkdir -p "$mectov_directory"
    magick "$mectov_root/assets/mectov.png" \
        -colorspace sRGB -filter Lanczos -resize "${mectov_size}x${mectov_size}" \
        -background none -gravity center -extent "${mectov_size}x${mectov_size}" \
        -strip -depth 8 -define png:color-type=6 \
        "$mectov_directory/me.silverl.mectov.png"
done

printf 'Generated desktop icons from assets/mectov.png.\n'
