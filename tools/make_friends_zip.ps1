<#
.SYNOPSIS
Builds the manager and zips it with the friends' README into target\w2rpm-friends.zip.
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    cargo build --release -p w2rpm-gui
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
    $stage = Join-Path $root 'target\friends'
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $stage | Out-Null
    Copy-Item target\release\w2rpm-gui.exe $stage
    Copy-Item tools\release\README.txt $stage
    Copy-Item THIRD_PARTY_NOTICES.md $stage
    $zip = Join-Path $root 'target\w2rpm-friends.zip'
    Remove-Item $zip -Force -ErrorAction SilentlyContinue
    Compress-Archive -Path "$stage\*" -DestinationPath $zip
    Write-Host "wrote $zip"
} finally { Pop-Location }
