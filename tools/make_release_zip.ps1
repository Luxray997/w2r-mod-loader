<#
.SYNOPSIS
Builds W2R Mod Loader and zips it with the README into target\W2R-Mod-Loader.zip.
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    cargo build --release -p w2r-loader-gui
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
    $stage = Join-Path $root 'target\release-pkg'
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $stage | Out-Null
    Copy-Item target\release\w2r-loader-gui.exe (Join-Path $stage 'W2R Mod Loader.exe')
    Copy-Item tools\release\README.txt $stage
    Copy-Item THIRD_PARTY_NOTICES.md $stage
    $zip = Join-Path $root 'target\W2R-Mod-Loader.zip'
    Remove-Item $zip -Force -ErrorAction SilentlyContinue
    Compress-Archive -Path "$stage\*" -DestinationPath $zip
    Write-Host "wrote $zip"
} finally { Pop-Location }
