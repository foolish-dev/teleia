# Teleia (τέλεια) installer for Windows — auto-detects platform, downloads
# the latest prebuilt binary from GitHub Releases, falls back to a cargo
# source build if no release exists for this platform.
# Usage:
#   irm https://raw.githubusercontent.com/foolish-dev/teleia/dev/install.ps1 | iex
# Overrides:
#   $env:PREFIX = "C:\Tools"      # default: $env:USERPROFILE\.local\bin
#   $env:TAG = "v0.2.0"           # pin a release tag (default: latest)
#   $env:FROM_SOURCE = "1"        # skip prebuilt download; cargo build instead
#   $env:BRANCH = "main"          # source-build branch (default: dev)
#   $env:NO_PATH = "1"            # don't touch your user PATH
#   $env:NO_OLLAMA_HINT = "1"     # suppress the post-install Ollama nudge
#   $env:AUTO_INSTALL = "1"       # bootstrap missing build deps (rustup) without prompting
#   $env:NO_AUTO_INSTALL = "1"    # never bootstrap; fail fast with manual instructions

$ErrorActionPreference = 'Stop'

$Prefix = if ($env:PREFIX) { $env:PREFIX } else { Join-Path $env:USERPROFILE '.local\bin' }
$Branch = if ($env:BRANCH) { $env:BRANCH } else { 'dev' }
$Tag    = if ($env:TAG)    { $env:TAG }    else { 'latest' }
$Repo   = 'https://github.com/foolish-dev/teleia'

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("teleia-" + [System.Guid]::NewGuid().ToString('N'))
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
        "$Repo/releases/latest/download/teleia-$target.exe"
    } else {
        "$Repo/releases/download/$Tag/teleia-$target.exe"
    }
    Write-Host "fetching τέλεια binary ($target)..."
    try {
        Invoke-WebRequest -Uri $url -OutFile (Join-Path $Tmp 'teleia.exe') -UseBasicParsing
        return $true
    } catch {
        return $false
    }
}

# Verify the downloaded binary against the release's SHA256SUMS. Releases
# without that file skip the check (with a warning) so older tags still
# install; a present-but-mismatched checksum aborts.
function Verify-Prebuilt($target) {
    $sumsUrl = if ($Tag -eq 'latest') {
        "$Repo/releases/latest/download/SHA256SUMS"
    } else {
        "$Repo/releases/download/$Tag/SHA256SUMS"
    }
    $sumsPath = Join-Path $Tmp 'SHA256SUMS'
    # A 404 is the honest "this release has no SHA256SUMS" answer. Anything
    # else — DNS, a refused connection, a captive portal, a 5xx — is the
    # network failing, and skipping the check there installs an unverified
    # binary exactly when something is wrong.
    try {
        Invoke-WebRequest -Uri $sumsUrl -OutFile $sumsPath -UseBasicParsing
    } catch {
        $code = $_.Exception.Response.StatusCode.value__
        if ($code -eq 404) {
            Write-Warning "no SHA256SUMS for this release — skipping integrity check"
            return
        }
        Write-Error "could not fetch SHA256SUMS ($($_.Exception.Message))"
        Write-Error "refusing to install unverified — re-run when the network is healthy."
        exit 1
    }
    $asset = "teleia-$target.exe"
    $pattern = "  " + [regex]::Escape($asset) + "$"
    $line = Select-String -Path $sumsPath -Pattern $pattern | Select-Object -First 1
    if (-not $line) {
        Write-Warning "$asset not listed in SHA256SUMS — skipping integrity check"
        return
    }
    $expected = (($line.Line -split '\s+')[0]).ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp 'teleia.exe')).Hash.ToLower()
    if ($expected -ne $actual) {
        throw "checksum mismatch for $asset (expected $expected, got $actual) — refusing to install"
    }
    Write-Host "integrity: sha256 verified"
}

# Bootstrap rustup non-interactively when cargo is missing. Honors
# $env:AUTO_INSTALL = '1' (skip the prompt and go) and
# $env:NO_AUTO_INSTALL = '1' (skip the bootstrap entirely). When neither
# is set we Read-Host; the subsequent Need cargo surfaces the manual
# instructions if the user declines.
function Ensure-Cargo {
    if (Get-Command cargo -ErrorAction SilentlyContinue) { return $true }
    if ($env:NO_AUTO_INSTALL -eq '1') { return $false }
    if ($env:AUTO_INSTALL -ne '1') {
        $reply = Read-Host 'cargo not found. install rustup non-interactively? [Y/n]'
        if ($reply -match '^(n|N|no|No|NO)$') { return $false }
    }
    $rustupArch = switch -Regex ($env:PROCESSOR_ARCHITECTURE) {
        '^AMD64$|^x86_64$' { 'x86_64' }
        '^ARM64$'          { 'aarch64' }
        default            { 'i686' }
    }
    $rustupInit = Join-Path $Tmp 'rustup-init.exe'
    Write-Host "installing rustup (stable toolchain, minimal profile)..."
    Invoke-WebRequest -Uri "https://win.rustup.rs/$rustupArch" -OutFile $rustupInit -UseBasicParsing
    & $rustupInit -y --default-toolchain stable --profile minimal --no-modify-path
    if ($LASTEXITCODE -ne 0) { return $false }
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    $env:Path = "$cargoBin;$env:Path"
    return [bool](Get-Command cargo -ErrorAction SilentlyContinue)
}

function From-Source {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Ensure-Cargo | Out-Null
    }
    Need cargo 'install Rust via https://rustup.rs'
    Need git   'install git via https://git-scm.com/download/win'

    Write-Host "fetching τέλεια source ($Branch)..."
    git clone --depth 1 --branch $Branch "$Repo.git" (Join-Path $Tmp 'src')
    if ($LASTEXITCODE -ne 0) { throw "git clone failed" }

    Write-Host "building..."
    Push-Location (Join-Path $Tmp 'src')
    try {
        cargo build --release --bin teleia
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally {
        Pop-Location
    }
    Copy-Item -Force (Join-Path $Tmp 'src\target\release\teleia.exe') (Join-Path $Tmp 'teleia.exe')
}

try {
    $done = $false
    if ($env:FROM_SOURCE -eq '1') {
        From-Source
        $done = $true
    } else {
        $target = Detect-Target
        if ($target -and (Try-Prebuilt $target)) {
            Verify-Prebuilt $target
            $done = $true
        }
    }
    if (-not $done) {
        Write-Host "no prebuilt for this platform — falling back to source build"
        From-Source
    }

    New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    $Dst = Join-Path $Prefix 'teleia.exe'
    Copy-Item -Force (Join-Path $Tmp 'teleia.exe') $Dst
    Write-Host "installed: $Dst"

    # --- automatic setup -------------------------------------------------

    # verify: catch arch mismatches / broken artifacts early.
    & $Dst --version *> $null
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "'$Dst --version' didn't run cleanly; binary may not match this platform"
    }

    # PATH setup: update User-scope PATH, idempotent. $env:NO_PATH=1 opts out.
    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $Parts = if ($UserPath) { $UserPath -split ';' } else { @() }
    $OnPath = $Parts -contains $Prefix
    if (-not $OnPath -and $env:NO_PATH -ne '1') {
        [Environment]::SetEnvironmentVariable('Path', (($Parts + $Prefix) -join ';'), 'User')
        Write-Host "added $Prefix to your user PATH — open a new shell to pick it up"
    }

    # Ollama hint.
    if ($env:NO_OLLAMA_HINT -ne '1') {
        if (Get-Command ollama -ErrorAction SilentlyContinue) {
            Write-Host "next: 'ollama pull hf.co/FoolDev/Thanatos-27B:Q4_K_M' to grab the default local model, or run 'teleia --model claude-fable-5'"
        } else {
            Write-Host "next: install Ollama (https://ollama.com) for local models, or run 'teleia --model claude-fable-5'"
        }
    }
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
