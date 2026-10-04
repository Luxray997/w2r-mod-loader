<#
.SYNOPSIS
Developer install of the w2rpm host into the game, with the hello-world fixture plugin.

.DESCRIPTION
Builds w2r_core.dll (release, i686) and copies it into the game's x86 folder as version.dll.
Sets up the host root under Saved Games\Warcraft2Remastered\w2r with the hello_plugin fixture
(plugin.json, DLL, pinned hash, chat.write approved). Then launch the game, enter a multiplayer
lobby, and read logs\w2r.log: the build guard should report PASSED (197/197) and the lobby chat
should show "hello from w2rpm".

.PARAMETER Uninstall
Removes version.dll from the game folder (the Saved Games root is left alone).

.PARAMETER WithCrasher
Also installs the crash_plugin fixture, which faults on lobby entry; the log should show it
disabled while the game and hello_plugin carry on.

.PARAMETER GameDir
The game's x86 folder. Defaults to D:\Games\Warcraft II Remastered\x86.
#>
param(
    [switch]$Uninstall,
    [switch]$WithCrasher,
    [string]$GameDir = "D:\Games\Warcraft II Remastered\x86"
)
$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
$target = Join-Path $GameDir "version.dll"

if ($Uninstall) {
    if (Test-Path $target) { Remove-Item $target; Write-Host "removed $target" } else { Write-Host "nothing installed at $target" }
    exit 0
}

if (-not (Test-Path (Join-Path $GameDir "Warcraft II.exe"))) {
    throw "Warcraft II.exe not found in $GameDir"
}

Push-Location $repo
try {
    cargo build --release -p w2r-core --target i686-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw "host build failed" }
    cargo build --release -p w2r-test-plugins --examples --target i686-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw "fixture build failed" }
} finally {
    Pop-Location
}

$dll = Join-Path $repo "target\i686-pc-windows-msvc\release\w2r_core.dll"
$examples = Join-Path $repo "target\i686-pc-windows-msvc\release\examples"
Copy-Item $dll $target -Force
Write-Host "installed $target"

# Saved Games may be relocated; ask the shell, fall back to the default.
$savedGames = $null
try {
    $shellFolders = Get-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders"
    $savedGames = [Environment]::ExpandEnvironmentVariables($shellFolders.'{4C5C32FF-BB9D-43B0-B5B4-2D72E54EAAA4}')
} catch {}
if (-not $savedGames) { $savedGames = Join-Path $env:USERPROFILE "Saved Games" }
$root = Join-Path $savedGames "Warcraft2Remastered\w2r"
foreach ($sub in "plugins", "config", "cache", "logs") {
    New-Item -ItemType Directory -Force (Join-Path $root $sub) | Out-Null
}

function Install-Fixture([string]$Id, [string]$Example, [string[]]$Capabilities) {
    $folder = Join-Path $root "plugins\$Id"
    New-Item -ItemType Directory -Force $folder | Out-Null
    $entry = "$Example.dll"
    Copy-Item (Join-Path $examples $entry) (Join-Path $folder $entry) -Force
    $capsJson = ($Capabilities | ForEach-Object { '"' + $_ + '"' }) -join ", "
    $manifest = @"
{
  "schema": 1,
  "id": "$Id",
  "name": "$Example (fixture)",
  "version": "1.0.0",
  "author": "w2rpm tests",
  "description": "Development fixture installed by tools/install_dev.ps1.",
  "entry": "$entry",
  "abi": 1,
  "game_builds": ["1.0.2.2818"],
  "capabilities": [$capsJson],
  "affects_simulation": false,
  "dependencies": []
}
"@
    [IO.File]::WriteAllText((Join-Path $folder "plugin.json"), $manifest, [Text.UTF8Encoding]::new($false))
    $hash = (Get-FileHash -Algorithm SHA256 (Join-Path $folder $entry)).Hash.ToLower()
    return @"
    "$Id": {
      "version": "1.0.0",
      "enabled": true,
      "sha256": "$hash",
      "approved_capabilities": [$capsJson]
    }
"@
}

$entries = @(Install-Fixture "hello-plugin" "hello_plugin" @("chat.write"))
if ($WithCrasher) { $entries += Install-Fixture "crash-plugin" "crash_plugin" @() }
$state = "{`n  `"schema`": 1,`n  `"plugins`": {`n" + ($entries -join ",`n") + "`n  }`n}`n"
[IO.File]::WriteAllText((Join-Path $root "config\state.json"), $state, [Text.UTF8Encoding]::new($false))

Write-Host "host root: $root"
Write-Host "now launch the game, enter a multiplayer lobby, then read $root\logs\w2r.log"
