#!/usr/bin/env bash
set -euo pipefail

# Run on the platform being packaged; the resulting binary uses the host architecture.
repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"
case "$(uname -s)" in
    Darwin) platform=macos ;;
    Linux) platform=linux ;;
    *) echo "This script requires macOS or Linux." >&2; exit 1 ;;
esac
command -v python3 >/dev/null || { echo "python3 is required." >&2; exit 1; }
metadata=$(cargo metadata --locked --no-deps --format-version 1)
version=$(printf '%s' "$metadata" | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "nya-scraping"))')
target_dir=$(printf '%s' "$metadata" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
target=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$target" ] || { echo "Could not determine Rust host target." >&2; exit 1; }
cargo build --locked --release -p nya-scraping --target "$target"
output_dir="$repo_root/build/packages"
mkdir -p "$output_dir"
staging=$(mktemp -d "${TMPDIR:-/tmp}/nya-package.XXXXXXXX")
trap 'rm -rf "$staging"' EXIT
name="NyaScraping-$version-$platform-$target"

if [ "$platform" = macos ]; then
    app="$staging/$name/NyaScraping.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "$target_dir/$target/release/nya-scraping" "$app/Contents/MacOS/nya-scraping"
    cp LICENSE "$staging/$name/LICENSE"
    # iconutil requires a complete iconset, generated from the application's PNG.
    iconset="$staging/AppIcon.iconset"
    mkdir -p "$iconset"
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" apps/desktop/assets/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
        double=$((size * 2))
        sips -z "$double" "$double" apps/desktop/assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
    cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>nya-scraping</string>
<key>CFBundleIdentifier</key><string>com.nyascraping.desktop</string>
<key>CFBundleName</key><string>NyaScraping</string>
<key>CFBundleDisplayName</key><string>NyaScraping</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
    archive="$output_dir/$name.zip"
    ditto -c -k --sequesterRsrc --keepParent "$staging/$name" "$archive"
else
    package="$staging/$name"
    mkdir -p "$package/bin" "$package/share/applications" "$package/share/pixmaps"
    cp "$target_dir/$target/release/nya-scraping" "$package/bin/nya-scraping"
    cp LICENSE "$package/LICENSE"
    cp apps/desktop/assets/icon.png "$package/share/pixmaps/nya-scraping.png"
    cat > "$package/share/applications/nya-scraping.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=NyaScraping
Comment=Embedded Debug Studio
Exec=nya-scraping
Icon=nya-scraping
Terminal=false
Categories=Development;Debugger;
DESKTOP
    archive="$output_dir/$name.tar.gz"
    tar -C "$staging" -czf "$archive" "$name"
fi
printf 'Package created: %s\n' "$archive"
