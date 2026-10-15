<#
.SYNOPSIS
Builds the core mod and writes the two files that publish it: w2r-core-<version>.dll and core.json.

.DESCRIPTION
Builds w2r_core.dll (release, i686) and writes, into OutDir, the DLL as w2r-core-<version>.dll and
a core.json that points at it (docs/core.md). Upload both next to index.json (the gh-pages branch
of w2r-plugins). Players get it with `w2rpm core update` or the Core mod section of the GUI.

Bump `version` in crates\w2r-core\Cargo.toml first: the manager only offers a core whose version is
higher than the installed one. The serial must also rise with every publish (default: the current
Unix time), or managers refuse the file as a rollback.

.PARAMETER OutDir
Where the files go. Defaults to target\core-release.

.PARAMETER BaseUrl
Where OutDir is served from (https only). Defaults to the w2r-plugins Pages site.

.PARAMETER Serial
The core.json serial. Defaults to the current Unix time in seconds.

.PARAMETER MinManager
The oldest w2rpm that may install this core. Raise it when the core needs a newer manager.
#>
param(
    [string]$OutDir = "",
    [string]$BaseUrl = "https://hastinbe.github.io/w2r-plugins",
    [int64]$Serial = 0,
    [string]$MinManager = "0.1.0"
)
$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $repo "target\core-release" }
if ($Serial -le 0) { $Serial = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() }

$cargo = Get-Content (Join-Path $repo "crates\w2r-core\Cargo.toml") -Raw
if ($cargo -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') { throw "no plain x.y.z version in w2r-core/Cargo.toml" }
$version = $Matches[1]
$mod = Get-Content (Join-Path $repo "crates\w2r-core\src\target\mod.rs") -Raw
if ($mod -notmatch 'GAME_VERSION: &str = "([0-9.]+)"') { throw "GAME_VERSION not found" }
$build = $Matches[1]

Push-Location $repo
try { cargo build --release -p w2r-core --target i686-pc-windows-msvc; if ($LASTEXITCODE) { throw "cargo build failed" } }
finally { Pop-Location }

$built = Join-Path $repo "target\i686-pc-windows-msvc\release\w2r_core.dll"
New-Item -ItemType Directory -Force $OutDir | Out-Null
$name = "w2r-core-$version.dll"
$dll = Join-Path $OutDir $name
Copy-Item $built $dll -Force

# The DLL must carry the version it is published as (the manager checks it).
$bytes = [IO.File]::ReadAllBytes($dll)
$text = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
if (-not $text.Contains("W2R-CORE-VERSION:$version;")) { throw "the built DLL does not carry version $version" }

$hash = (Get-FileHash $dll -Algorithm SHA256).Hash.ToLower()
$json = '{{"schema":1,"serial":{0},"version":"{1}","url":"{2}/{3}","sha256":"{4}","size":{5},"min_manager":"{6}","game_builds":["{7}"]}}' -f `
    $Serial, $version, $BaseUrl.TrimEnd('/'), $name, $hash, $bytes.Length, $MinManager, $build
[IO.File]::WriteAllText((Join-Path $OutDir "core.json"), $json, (New-Object Text.UTF8Encoding($false)))
Write-Host "core $version (serial $Serial, game $build) written to $OutDir"
Write-Host "upload $name and core.json next to index.json"
