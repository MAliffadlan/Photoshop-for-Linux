#!/usr/bin/env python3
"""Verify binary payloads and their accompanying rebuildable source archive."""

import hashlib
import io
import os
import platform
import re
import subprocess
import shutil
import tarfile
import tempfile
import tomllib
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent
VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
NAME = f"mectov-{VERSION}-linux-{platform.machine()}"
SOURCE_NAME = f"mectov-{VERSION}-source"


def check_checksum(package):
    digest, filename = package.with_name(package.name + ".sha256").read_text().split()
    assert filename == package.name, f"Incorrect checksum filename: {filename}"
    with package.open("rb") as stream:
        assert hashlib.file_digest(stream, "sha256").hexdigest() == digest, package


def check_files(prefix, portable=False):
    expected = {
        "bin/mectov",
        "share/applications/me.silverl.mectov.desktop",
        "share/mime/packages/me.silverl.mectov.xml",
        "share/licenses/mectov/LICENSE",
        "share/licenses/mectov/rawler-LGPL-2.1.txt",
        "share/licenses/mectov/Inter-LICENSE.txt",
        "share/licenses/mectov/egui-winit/LICENSE-MIT",
        "share/licenses/mectov/egui-winit/LICENSE-APACHE",
    }
    documents = [
        "README.md",
        "USAGE.md",
        "SHORTCUTS.md",
        "RAW.md",
        "THIRD_PARTY.md",
        "SOURCES.md",
        "copyright",
    ]
    expected.update(f"share/doc/mectov/{name}" for name in documents)
    icons = list((ROOT / "assets/icons").glob("hicolor/*/apps/*.png"))
    expected.update(
        f"share/icons/{path.relative_to(ROOT / 'assets/icons')}" for path in icons
    )
    if portable:
        expected.add("scripts/install.sh")
    actual = {
        path.relative_to(prefix).as_posix()
        for path in prefix.rglob("*")
        if path.is_file()
    }
    assert actual == expected, (
        f"Unexpected files: {actual - expected}; missing: {expected - actual}"
    )
    allowed_directories = {parent for name in expected for parent in Path(name).parents}
    for path in prefix.rglob("*"):
        assert not path.is_symlink(), f"Unexpected symlink: {path}"
        if path.is_dir():
            assert path.relative_to(prefix) in allowed_directories, path
        required = 0o005 if path.is_dir() else 0o004
        assert path.stat().st_mode & required == required, (
            f"Not publicly readable: {path}"
        )
    assert (prefix / "bin/mectov").stat().st_mode & 0o777 == 0o755
    for original in icons:
        installed = prefix / "share/icons" / original.relative_to(ROOT / "assets/icons")
        assert installed.read_bytes() == original.read_bytes(), installed
    documentation = prefix / "share/doc/mectov"
    for name in documents:
        for link in re.findall(
            r"\[[^\]]*\]\(([^)]+)\)", (documentation / name).read_text()
        ):
            target = urlsplit(link)
            if not target.scheme and not target.netloc and target.path:
                assert (documentation / target.path).exists(), (
                    f"Broken link in {name}: {link}"
                )
    source_notice = (documentation / "SOURCES.md").read_text()
    assert f"/releases/download/v{VERSION}/{SOURCE_NAME}.tar.gz" in source_notice
    assert f"/releases/download/v{VERSION}/{SOURCE_NAME}.tar.gz.sha256" in source_notice
    subprocess.run(
        [
            "desktop-file-validate",
            prefix / "share/applications/me.silverl.mectov.desktop",
        ],
        check=True,
    )
    actual_version = subprocess.check_output(
        [prefix / "bin/mectov", "--version"], text=True
    ).strip()
    assert actual_version == f"mectov {VERSION}", actual_version


def check_appimage(package, destination):
    """Validate the AppImage structure and verify its payload contents."""
    with package.open("rb") as stream:
        image = stream.read()
    assert image[:4] == b"\x7fELF", "Missing ELF header"
    assert image[8:11] == b"AI\x02", "Missing AppImage type 2 signature"
    # The first squashfs magic can be false bytes inside compressed ELF
    # sections, so accept the first occurrence with a sane superblock.
    superblock = None
    start = 0
    while True:
        magic = image.find(b"hsqs", start)
        if magic == -1:
            break
        inodes = int.from_bytes(image[magic + 4 : magic + 8], "little")
        version = int.from_bytes(image[magic + 28 : magic + 30], "little")
        if magic % 4 == 0 and 0 < inodes < 1_000_000 and version == 4:
            superblock = magic
            break
        start = magic + 1
    assert superblock is not None, "Missing valid squashfs superblock"
    destination.mkdir()
    embedded = destination / "embedded.img"
    embedded.write_bytes(image[superblock:])
    subprocess.run(
        ["unsquashfs", "-no-progress", "-d", str(destination / "squash"), str(embedded)],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    check_files(destination / "squash" / "usr", portable=True)
    apprun = destination / "squash" / "AppRun"
    assert apprun.stat().st_mode & 0o111, "AppRun is not executable"
    desktop = destination / "squash" / "me.silverl.mectov.desktop"
    subprocess.run(["desktop-file-validate", desktop], check=True)
    shutil.rmtree(destination / "squash")
    embedded.unlink()
    print(f"Verified {package.name}: runtime files, AppRun, desktop entry")


def check_source(temporary):
    package = ROOT / "dist" / f"{SOURCE_NAME}.tar.gz"
    check_checksum(package)
    with tarfile.open(package) as archive:
        archive.extractall(temporary, filter="data")
    source = temporary / SOURCE_NAME
    for name in (
        "Cargo.toml",
        "Cargo.lock",
        "vendor/rawler/Cargo.toml",
        "vendor/egui-winit/Cargo.toml",
        "assets/mectov.png",
        "scripts/package.sh",
        "docs/DEVELOPMENT.md",
        "LICENSE",
        "THIRD_PARTY.md",
    ):
        assert (source / name).is_file(), f"Missing rebuild source: {name}"
    for original in (ROOT / "src").rglob("*"):
        if original.suffix in (".rs", ".wgsl"):
            assert (
                source / original.relative_to(ROOT)
            ).read_bytes() == original.read_bytes(), original
    manifest = tomllib.loads((source / "Cargo.toml").read_text())
    assert manifest["package"]["version"] == VERSION
    assert manifest["patch"]["crates-io"]["rawler"]["path"] == "vendor/rawler"
    host = (
        subprocess.check_output(["rustc", "-vV"], text=True)
        .split("host: ")[1]
        .splitlines()[0]
    )
    subprocess.run(
        [
            "cargo",
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            host,
            "--manifest-path",
            source / "Cargo.toml",
        ],
        stdout=subprocess.DEVNULL,
        check=True,
    )
    print(
        f"Verified {package.name}: matching sources, vendored dependencies, resolved lockfile"
    )


def main():
    os.umask(0o022)
    with tempfile.TemporaryDirectory(prefix="mectov-package-check-") as directory:
        temporary = Path(directory)
        check_source(temporary)
        for extension in ("tar.gz", "deb", "rpm", "AppImage"):
            package = ROOT / "dist" / f"{NAME}.{extension}"
            check_checksum(package)
            destination = temporary / extension
            if extension == "tar.gz":
                with tarfile.open(package) as archive:
                    archive.extractall(destination, filter="data")
                stage = destination / NAME
                check_files(stage, portable=True)
                installed = temporary / "portable installation"
                subprocess.run([stage / "scripts/install.sh", installed], check=True)
                for path in (stage / "share").rglob("*"):
                    if path.is_file():
                        assert (
                            installed / path.relative_to(stage)
                        ).read_bytes() == path.read_bytes(), path
                subprocess.run([installed / "bin/mectov", "--version"], check=True)
                print(
                    f"Verified {package.name}: runtime files, documentation, portable installation"
                )
                continue
            if extension == "AppImage":
                check_appimage(package, destination)
                continue
            if extension == "deb":
                metadata = subprocess.check_output(
                    ["dpkg-deb", "--field", package], text=True
                )
                assert f"Version: {VERSION.replace('-', '~', 1)}-1\n" in metadata
                assert "libc6 (>= " in metadata and "libvulkan1" in metadata
                controls = subprocess.check_output(
                    ["dpkg-deb", "--ctrl-tarfile", package]
                )
                with tarfile.open(fileobj=io.BytesIO(controls)) as archive:
                    for name in ("./postinst", "./postrm"):
                        assert archive.getmember(name).mode == 0o755
                        assert (
                            b"update-mime-database" in archive.extractfile(name).read()
                        )
                data = subprocess.check_output(["dpkg-deb", "--fsys-tarfile", package])
                with tarfile.open(fileobj=io.BytesIO(data)) as archive:
                    assert all(
                        member.uid == member.gid == 0 for member in archive.getmembers()
                    )
                subprocess.run(
                    ["dpkg-deb", "--extract", package, destination], check=True
                )
            else:
                metadata = subprocess.check_output(
                    ["rpm", "-qp", "--requires", package], text=True
                )
                assert "libc.so.6(GLIBC_" in metadata and "libvulkan.so.1" in metadata
                scripts = subprocess.check_output(
                    ["rpm", "-qp", "--scripts", package], text=True
                )
                assert scripts.count("gtk-update-icon-cache -q -f -t") == 2
                owners = subprocess.check_output(
                    [
                        "rpm",
                        "-qp",
                        "--qf",
                        "[%{FILEUSERNAME}:%{FILEGROUPNAME}\n]",
                        package,
                    ],
                    text=True,
                )
                assert set(owners.splitlines()) == {"root:root"}
                destination.mkdir()
                data = subprocess.check_output(["rpm2cpio", package])
                subprocess.run(
                    [
                        "cpio",
                        "--extract",
                        "--make-directories",
                        "--quiet",
                        "--no-absolute-filenames",
                    ],
                    input=data,
                    cwd=destination,
                    check=True,
                )
            check_files(destination / "usr")
            print(
                f"Verified {package.name}: runtime files, documentation, metadata, ownership"
            )
    print("All binary packages, source archive, and checksums passed.")


if __name__ == "__main__":
    main()
