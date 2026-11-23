#Requires -Version 7
# Raises the patch version unless the build is a release, then builds the
# release binaries, pairs the window with its gozlerd.exe (Pair.ps1), and
# packages them with Setup.iss into
# target\installer\gozler-<version>-x64-Setup.exe.
param(
    # ISCC.exe from Inno Setup 7.1 or later. By default, the one on PATH or in
    # a standard Inno Setup 7 installation folder.
    [string] $Compiler
)

$ErrorActionPreference = 'Stop'

if (-not $Compiler) {
    $Compiler = @(
        (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
        Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 7\ISCC.exe'
        Join-Path $env:ProgramFiles 'Inno Setup 7\ISCC.exe'
        Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 7\ISCC.exe'
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
    if (-not $Compiler) { throw 'Inno Setup 7 was not found. Install it or pass -Compiler.' }
}

Push-Location (Split-Path $PSScriptRoot)
try {
    # Each local run raises the patch version in Cargo.toml, so every setup it
    # builds upgrades the one before. A release, which the release workflow
    # marks with GOZLER_RELEASE=1, builds the version its tag names.
    if ($env:GOZLER_RELEASE -ne '1') {
        $manifest = Join-Path (Get-Location) 'Cargo.toml'
        $text = [IO.File]::ReadAllText($manifest)
        $version = [regex]::Match($text, '(?m)^version = "(\d+)\.(\d+)\.(\d+)"')
        if (-not $version.Success) { throw 'Cargo.toml has no workspace version.' }
        $next = '{0}.{1}.{2}' -f $version.Groups[1].Value, $version.Groups[2].Value,
            ([int]$version.Groups[3].Value + 1)
        $text = $text.Remove($version.Index, $version.Length).Insert($version.Index, "version = `"$next`"")
        [IO.File]::WriteAllText($manifest, $text)
        Write-Host "Cargo.toml version $next"
    }

    $package = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
        Where-Object name -eq 'gozler-daemon'
    if (-not $package) { throw 'cargo metadata did not list gozler-daemon.' }

    & (Join-Path (Get-Location) 'build.ps1') -Release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & (Join-Path $PSScriptRoot 'Pair.ps1')

    $version = [regex]::Escape($package.version)
    $history = Get-Content -LiteralPath 'docs/WHATS-NEW.md' -Raw
    $notes = [regex]::Match($history, "(?ms)^## Gozler ${version}[ \t]*\r?\n(.*?)(?=^## |\z)").Groups[1].Value.Trim()
    $notesArgs = @()
    if ($notes) {
        $output = New-Item -ItemType Directory -Path 'target/release-notes' -Force
        Set-Content -LiteralPath (Join-Path $output.FullName "gozler-v$($package.version).md") -Value $notes -Encoding utf8
        $page = Join-Path $output.FullName 'WhatsNew.txt'
        $plain = $notes -replace '(?m)^#{1,6} '
        $plain = $plain -replace '`([^`\r\n]+)`|\*\*([^*\r\n]+)\*\*', '$1$2'
        $plain = $plain -replace '\[([^\]\r\n]+)\]\((https?://[^)\s]+)\)', '$1 ($2)'
        Set-Content -LiteralPath $page -Encoding utf8BOM -Value "Gozler $($package.version)`n`n$plain"
        $notesArgs = @("/DWhatsNewFile=$page")
    }

    # The first author, the company that develops gozler, is the publisher.
    & $Compiler /Qp "/DAppVersion=$($package.version)" "/DAppPublisher=$($package.authors[0])" `
        "/DAppUrl=$($package.repository)" @notesArgs installer\Setup.iss
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    Pop-Location
}
