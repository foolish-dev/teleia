# Telia (τέλεια) installer for Windows — clones the repo, builds with cargo,
# drops `telia.exe` into $Prefix.
# Usage:
#   irm https://raw.githubusercontent.com/foolish-dev/telia/dev/install.ps1 | iex
# Overrides:
#   $env:PREFIX = "C:\Tools"   # default: $env:USERPROFILE\.local\bin
#   $env:BRANCH = "main"       # default: dev

$ErrorActionPreference = 'Stop'

$Prefix = if ($env:PREFIX) { $env:PREFIX } else { Join-Path $env:USERPROFILE '.local\bin' }
$Branch = if ($env:BRANCH) { $env:BRANCH } else { 'dev' }
$Repo   = 'https://github.com/foolish-dev/telia.git'

function Need($cmd, $hint) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        throw "error: $cmd not found in PATH. $hint"
    }
}

Need cargo 'install Rust via https://rustup.rs'
Need git   'install git via https://git-scm.com/download/win'

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("telia-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Write-Host "fetching τέλεια ($Branch)..."
    git clone --depth 1 --branch $Branch $Repo (Join-Path $Tmp 'telia')
    if ($LASTEXITCODE -ne 0) { throw "git clone failed" }

    Write-Host "building..."
    Push-Location (Join-Path $Tmp 'telia')
    try {
        cargo build --release --bin telia
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally {
        Pop-Location
    }

    New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    $Src = Join-Path $Tmp 'telia\target\release\telia.exe'
    $Dst = Join-Path $Prefix 'telia.exe'
    Copy-Item -Force $Src $Dst

    Write-Host "installed: $Dst"

    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $OnPath = $UserPath -and (($UserPath -split ';') -contains $Prefix)
    if (-not $OnPath) {
        Write-Host "note: $Prefix is not on your PATH"
    }
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
