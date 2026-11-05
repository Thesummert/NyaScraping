$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
$staging = $null
try {
    $metadataJson = cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    $metadata = $metadataJson | ConvertFrom-Json
    $version = ($metadata.packages | Where-Object { $_.name -eq 'nya-scraping' }).version
    if (-not $version) { throw 'Desktop package not found in Cargo metadata' }
    $target = (rustc -vV | Where-Object { $_ -like 'host: *' }) -replace '^host: ', ''
    if ($LASTEXITCODE -ne 0 -or -not $target -or $target -notmatch 'windows') { throw 'This script requires a Windows Rust host toolchain' }
    cargo build --locked --release -p nya-scraping --target $target
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    $name = "NyaScraping-$version-windows-$target"
    $outputDir = Join-Path $repoRoot 'build/packages'
    New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
    $staging = Join-Path ([System.IO.Path]::GetTempPath()) ("nya-package-" + [guid]::NewGuid())
    $package = Join-Path $staging $name
    New-Item -ItemType Directory -Path $package | Out-Null
    Copy-Item (Join-Path $metadata.target_directory "$target/release/nya-scraping.exe") $package
    Copy-Item (Join-Path $repoRoot 'LICENSE') $package
    $archive = Join-Path $outputDir "$name.zip"
    Compress-Archive -Path $package -DestinationPath $archive -Force
    Write-Host "Package created: $archive"
} finally {
    if ($staging -and (Test-Path -LiteralPath $staging)) { Remove-Item -LiteralPath $staging -Recurse -Force }
    Pop-Location
}
