# Install klyro on Windows:
#   irm https://raw.githubusercontent.com/khushramnani/Klyro/main/install.ps1 | iex
# Set $env:KLYRO_INSTALL_DIR to choose the folder (default %LOCALAPPDATA%\klyro\bin).
$ErrorActionPreference = 'Stop'

$repo = 'khushramnani/Klyro'
$dir = if ($env:KLYRO_INSTALL_DIR) { $env:KLYRO_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'klyro\bin' }
$target = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
$base = "https://github.com/$repo/releases/latest/download"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("klyro-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    $zip = Join-Path $tmp 'klyro.zip'
    $haveRelease = $true
    try { Invoke-WebRequest "$base/klyro-$target.zip" -OutFile $zip -UseBasicParsing } catch { $haveRelease = $false }

    if ($haveRelease) {
        $sums = Join-Path $tmp 'SHA256SUMS'
        Invoke-WebRequest "$base/SHA256SUMS" -OutFile $sums -UseBasicParsing
        $line = Get-Content $sums | Where-Object { $_ -match " klyro-$target\.zip$" } | Select-Object -First 1
        $expected = if ($line) { ($line -split ' ')[0] } else { '' }
        $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
        if (-not $expected -or $expected -ne $actual) { throw 'klyro: checksum mismatch, not installing' }
        Expand-Archive $zip -DestinationPath $tmp -Force
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        Copy-Item (Join-Path $tmp 'klyro.exe') $dir -Force
    }
    elseif (Get-Command cargo -ErrorAction SilentlyContinue) {
        Write-Host 'klyro: no prebuilt binary yet; building from source with cargo (a few minutes)'
        cargo install --locked --git "https://github.com/$repo" klyro-cli
        if ($LASTEXITCODE -ne 0) { throw 'klyro: cargo install failed' }
        $dir = Join-Path $env:USERPROFILE '.cargo\bin'
    }
    else {
        throw 'klyro: no prebuilt binary yet, and no Rust toolchain to build one. Install Rust (https://rustup.rs) and run this again.'
    }
}
finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "klyro: installed $dir\klyro.exe"
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    $env:Path += ";$dir"
    Write-Host "klyro: added $dir to your user PATH (new terminals will see it)"
}
Write-Host "klyro: agent skill: https://raw.githubusercontent.com/$repo/main/skill/SKILL.md (e.g. save as ~/.claude/skills/klyro/SKILL.md)"
& (Join-Path $dir 'klyro.exe') doctor
