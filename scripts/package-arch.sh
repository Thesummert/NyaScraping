#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"
if [[ $(uname -s) != Linux || $(uname -m) != x86_64 ]]; then
    echo 'Arch packaging currently requires x86_64 Linux.' >&2
    exit 1
fi
if (( EUID == 0 )); then
    echo 'Run this script as a regular user; makepkg must not run as root.' >&2
    exit 1
fi
for tool in cargo rustc python3 tar makepkg pacman sha256sum; do
    command -v "$tool" >/dev/null || { echo "Required command not found: $tool" >&2; exit 1; }
done
if [[ $(rustc -vV | sed -n 's/^host: //p') != x86_64-unknown-linux-gnu ]]; then
    echo 'An x86_64-unknown-linux-gnu Rust toolchain is required.' >&2
    exit 1
fi
if [[ ! -f backends/NyaScrapingCore/Cargo.toml ]]; then
    echo 'Missing backend submodule. Run: git submodule update --init --recursive' >&2
    exit 1
fi
pkgrel=${NYA_ARCH_PKGREL:-1}
[[ $pkgrel =~ ^[1-9][0-9]*$ ]] || { echo 'NYA_ARCH_PKGREL must be a positive integer.' >&2; exit 1; }
metadata=$(cargo metadata --locked --no-deps --format-version 1)
version=$(printf '%s' "$metadata" | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "nya-scraping").replace("-", "_"))')
# Use Cargo's existing cache; relative overrides are resolved before changing directories.
export CARGO_TARGET_DIR
CARGO_TARGET_DIR=$(printf '%s' "$metadata" | python3 -c 'import json,sys; from pathlib import Path; import os; print(Path(os.environ.get("CARGO_TARGET_DIR") or json.load(sys.stdin)["target_directory"]).resolve())')
work_dir="$repo_root/build/arch"
export PKGDEST="$repo_root/build/packages"
# Keep makepkg's sources and staging inside this workspace, even with custom user config.
export SRCDEST="$work_dir" BUILDDIR="$work_dir" LOGDEST="$work_dir"
mkdir -p "$work_dir" "$PKGDEST"
source_name="nya-scraping-$version"
# Snapshot the working tree, including the backend submodule and uncommitted edits.
# Build outputs and VCS metadata are deliberately excluded.
tar -C "$repo_root" --exclude=.git --exclude=target --exclude=build \
    --exclude='*.log' --transform="s,^,$source_name/," \
    -czf "$work_dir/$source_name.tar.gz" \
    Cargo.toml Cargo.lock rust-toolchain.toml LICENSE README.md apps crates backends
cp packaging/arch/nya-scraping.desktop "$work_dir/nya-scraping.desktop"
python3 - "$repo_root/packaging/arch/PKGBUILD.in" "$work_dir" "$version" "$pkgrel" <<'PY'
import hashlib
from pathlib import Path
import sys

template, directory, version, pkgrel = sys.argv[1:]
directory = Path(directory)
def checksum(name):
    with (directory / name).open('rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()
text = Path(template).read_text()
for key, value in {
    '@VERSION@': version,
    '@PKGREL@': pkgrel,
    '@SOURCE_SHA256@': checksum(f'nya-scraping-{version}.tar.gz'),
    '@DESKTOP_SHA256@': checksum('nya-scraping.desktop'),
}.items():
    text = text.replace(key, value)
(directory / 'PKGBUILD').write_text(text)
PY
cd "$work_dir"
makepkg --printsrcinfo > .SRCINFO
# Validate declared dependencies; do not install them or invoke sudo automatically.
makepkg --force --cleanbuild
package_list=$(makepkg --packagelist)
while IFS= read -r package; do
    [[ -f "$package" ]] || { echo "Package not created: $package" >&2; exit 1; }
    (cd "$(dirname "$package")" && sha256sum "$(basename "$package")") > "$package.sha256"
    printf 'Package created: %s\nInstall with: sudo pacman -U %q\n' "$package" "$package"
done <<< "$package_list"
