# Telia (τέλεια) installer for Windows — auto-detects platform, downloads
# the latest prebuilt binary from GitHub Releases, falls back to a cargo
# source build if no release exists for this platform.
# Usage:
#   irm https://raw.githubusercontent.com/foolish-dev/telia/dev/install.ps1 | iex
# Overrides:
#   $env:PREFIX = "C:\Tools"      # default: $env:USERPROFILE\.local\bin
#   $env:TAG = "v0.2.0"           # pin a release tag (default: latest)
#   $env:FROM_SOURCE = "1"        # skip prebuilt download; cargo build instead
#   $env:BRANCH = "main"          # source-build branch (default: dev)

$ErrorActionPreference = 'Stop'

$Prefix = if ($env:PREFIX) { $env:PREFIX } else { Join-Path $env:USERPROFILE '.local\bin' }
$Branch = if ($env:BRANCH) { $env:BRANCH } else { 'dev' }
$Tag    = if ($env:TAG)    { $env:TAG }    else { 'latest' }
$Repo   = 'https://github.com/foolish-dev/telia'

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("telia-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp | Out-Null

function Need($cmd, $hint) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        throw "error: $cmd not found in PATH. $hint"
    }
}

function Detect-Target {
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch -Regex ($arch) {
        '^AMD64$|^x86_64$' { return 'x86_64-pc-windows-msvc' }
        '^ARM64$'          { return 'aarch64-pc-windows-msvc' }
        '^x86$|^X86$'      { return 'i686-pc-windows-msvc' }
    }
    return $null
}

function Try-Prebuilt($target) {
    $url = if ($Tag -eq 'latest') {
        "$Repo/releases/latest/download/telia-$target.exe"
    } else {
        "$Repo/releases/download/$Tag/telia-$target.exe"
    }
    Write-Host "fetching τέλεια binary ($target)..."
    try {
        Invoke-WebRequest -Uri $url -OutFile (Join-Path $Tmp 'telia.exe') -UseBasicParsing
        return $true
    } catch {
        return $false
    }
}

function From-Source {
    Need cargo 'install Rust via https://rustup.rs'
    Need git   'install git via https://git-scm.com/download/win'

    Write-Host "fetching τέλεια source ($Branch)..."
    git clone --depth 1 --branch $Branch "$Repo.git" (Join-Path $Tmp 'src')
    if ($LASTEXITCODE -ne 0) { throw "git clone failed" }

    Write-Host "building..."
    Push-Location (Join-Path $Tmp 'src')
    try {
        cargo build --release --bin telia
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally {
        Pop-Location
    }
    Copy-Item -Force (Join-Path $Tmp 'src\target\release\telia.exe') (Join-Path $Tmp 'telia.exe')
}

try {
    $done = $false
    if ($env:FROM_SOURCE -eq '1') {
        From-Source
        $done = $true
    } else {
        $target = Detect-Target
        if ($target -and (Try-Prebuilt $target)) {
            $done = $true
        }
    }
    if (-not $done) {
        Write-Host "no prebuilt for this platform — falling back to source build"
        From-Source
    }

    New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    $Dst = Join-Path $Prefix 'telia.exe'
    Copy-Item -Force (Join-Path $Tmp 'telia.exe') $Dst
    Write-Host "installed: $Dst"

    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $OnPath = $UserPath -and (($UserPath -split ';') -contains $Prefix)
    if (-not $OnPath) {
        Write-Host "note: $Prefix is not on your PATH"
    }
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
