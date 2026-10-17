<#
.SYNOPSIS
Builds the manager and writes the two things that publish it: w2r-mod-loader-<version>.exe and manager.json.

.DESCRIPTION
Builds the GUI (release), checks the exe carries the version of crates\w2r-loader-gui\Cargo.toml and
writes the exe as w2r-mod-loader-<version>.exe plus a manager.json that points at it (docs/manager.md,
"Newer manager"). Upload both next to index.json (the gh-pages branch of w2r-plugins). Managers that
read the file show an "Update now" button; they check size, SHA-256 and the version marker in the exe
before swapping it in. The zip from make_release_zip.ps1 is still what new users download.

Bump `version` in the GUI and lib Cargo.toml first: managers only offer a version above their own.

.PARAMETER OutDir
Where the files go. Defaults to target\manager-release.

.PARAMETER BaseUrl
Where OutDir is served from (https only). Defaults to the w2r-plugins Pages site.

.PARAMETER PageUrl
The page "Get it" opens for managers too old to update themselves.
#>
param(
    [string]$OutDir = "",
    [string]$BaseUrl = "https://hastinbe.github.io/w2r-plugins",
    [string]$PageUrl = "https://github.com/hastinbe/w2r-mod-loader/releases/latest"
)
$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $repo "target\manager-release" }

$cargo = Get-Content (Join-Path $repo "crates\w2r-loader-gui\Cargo.toml") -Raw
if ($cargo -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') { throw "no plain x.y.z version in w2r-loader-gui/Cargo.toml" }
$version = $Matches[1]

Push-Location $repo
try { cargo build --release -p w2r-loader-gui; if ($LASTEXITCODE) { throw "cargo build failed" } }
finally { Pop-Location }

New-Item -ItemType Directory -Force $OutDir | Out-Null
$name = "w2r-mod-loader-$version.exe"
$exe = Join-Path $OutDir $name
Copy-Item (Join-Path $repo "target\release\w2r-loader-gui.exe") $exe -Force

$bytes = [IO.File]::ReadAllBytes($exe)
$text = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
if (-not $text.Contains("W2R-LOADER-VERSION:$version;")) { throw "the built exe does not carry version $version" }

$hash = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLower()
$json = '{{"schema":1,"version":"{0}","url":"{1}","package_url":"{2}/{3}","sha256":"{4}","size":{5}}}' -f `
    $version, $PageUrl, $BaseUrl.TrimEnd('/'), $name, $hash, $bytes.Length
[IO.File]::WriteAllText((Join-Path $OutDir "manager.json"), $json, (New-Object Text.UTF8Encoding($false)))
Write-Host "manager $version written to $OutDir"
Write-Host "upload $name and manager.json next to index.json"
