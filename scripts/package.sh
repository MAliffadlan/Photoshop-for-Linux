#!/usr/bin/env bash
set -euo pipefail
umask 022

mectov_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$mectov_root"
mectov_format=${1:-archive}
case "$mectov_format" in
    archive|deb|rpm|all) ;;
    *) printf 'Usage: %s [archive|deb|rpm|all]\n' "$0" >&2; exit 1 ;;
esac
if [[ $# -gt 1 ]]; then
    printf 'Usage: %s [archive|deb|rpm|all]\n' "$0" >&2
    exit 1
fi
for mectov_tool in python3 strip; do
    command -v "$mectov_tool" >/dev/null || { printf 'Required tool: %s\n' "$mectov_tool" >&2; exit 1; }
done
if [[ "$mectov_format" != archive ]]; then
    python3 scripts/package-native.py --check-tools "$mectov_format"
fi
cargo build --release --locked
mectov_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
mectov_name="mectov-${mectov_version}-linux-$(uname -m)"
mkdir -p dist
mectov_temporary=$(mktemp -d "$mectov_root/dist/.package.XXXXXX")
trap 'rm -rf -- "$mectov_temporary"' EXIT
mectov_stage="$mectov_temporary/$mectov_name"
mkdir -p "$mectov_stage"/{bin,share,scripts}
install -m755 target/release/mectov "$mectov_stage/bin/mectov"
strip "$mectov_stage/bin/mectov"
cp -R assets/icons "$mectov_stage/share/"
install -Dm644 packaging/me.silverl.mectov.desktop "$mectov_stage/share/applications/me.silverl.mectov.desktop"
install -Dm644 packaging/me.silverl.mectov.xml "$mectov_stage/share/mime/packages/me.silverl.mectov.xml"
mectov_licenses="$mectov_stage/share/licenses/mectov"
install -Dm644 LICENSE "$mectov_licenses/LICENSE"
install -m644 licenses/rawler-LGPL-2.1.txt "$mectov_licenses/"
install -m644 assets/fonts/Inter-LICENSE.txt "$mectov_licenses/"
for mectov_license in LICENSE-MIT LICENSE-APACHE; do
    install -Dm644 "vendor/egui-winit/$mectov_license" "$mectov_licenses/egui-winit/$mectov_license"
done
python3 scripts/package-docs.py "$mectov_stage/share/doc/mectov" "$mectov_version"
install -m755 scripts/install.sh "$mectov_stage/scripts/"
chmod -R u=rwX,go=rX "$mectov_stage"

# Distribute matching rebuildable sources alongside every binary format.
mectov_source_name="mectov-${mectov_version}-source"
mectov_source="$mectov_temporary/$mectov_source_name"
mkdir -p "$mectov_source"
tar --exclude='*.env' --exclude='__pycache__' --exclude='*.pyc' --exclude='.git' \
    -cf - src assets vendor licenses scripts packaging docs .github |
    tar -xf - -C "$mectov_source"
install -m644 Cargo.toml Cargo.lock LICENSE README.md THIRD_PARTY.md "$mectov_source/"
mectov_host=$(rustc -vV | sed -n 's/^host: //p')
mectov_rawler_manifest=$(cargo metadata --locked --format-version 1 --filter-platform "$mectov_host" |
    python3 -c 'import json, sys; print(next(p["manifest_path"] for p in json.load(sys.stdin)["packages"] if p["name"] == "rawler"))')
mkdir -p "$mectov_source/vendor/rawler"
cp -R "$(dirname -- "$mectov_rawler_manifest")/." "$mectov_source/vendor/rawler/"
python3 - "$mectov_source/Cargo.toml" <<'PYTHON'
import sys
import tomllib
from pathlib import Path

manifest = Path(sys.argv[1])
text = manifest.read_text()
if "rawler" not in tomllib.loads(text).get("patch", {}).get("crates-io", {}):
    text = text.replace('[patch.crates-io]\n', '[patch.crates-io]\nrawler = { path = "vendor/rawler" }\n', 1)
manifest.write_text(text)
PYTHON
# Resolve the path patch now so recipients can rebuild with --locked.
cargo metadata --offline --format-version 1 --filter-platform "$mectov_host" \
    --manifest-path "$mectov_source/Cargo.toml" > /dev/null
tar -C "$mectov_temporary" -czf "dist/$mectov_source_name.tar.gz" "$mectov_source_name"
chmod 644 "dist/$mectov_source_name.tar.gz"
(cd dist && sha256sum "$mectov_source_name.tar.gz" > "$mectov_source_name.tar.gz.sha256")
printf 'Created %s/dist/%s.tar.gz\n' "$mectov_root" "$mectov_source_name"
if [[ "$mectov_format" == archive || "$mectov_format" == all ]]; then
    tar -C "$mectov_temporary" -czf "dist/$mectov_name.tar.gz" "$mectov_name"
    chmod 644 "dist/$mectov_name.tar.gz"
    (cd dist && sha256sum "$mectov_name.tar.gz" > "$mectov_name.tar.gz.sha256")
    printf 'Created %s/dist/%s.tar.gz\n' "$mectov_root" "$mectov_name"
fi
if [[ "$mectov_format" != archive ]]; then
    python3 scripts/package-native.py "$mectov_format" "$mectov_stage" "$mectov_version" "$mectov_root/dist"
fi
