$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repository = "NeoTamia/NTNook"
$target = "x86_64-pc-windows-msvc"
$archive = "nook-$target.zip"
$checksum = "$archive.sha256"
$version = $env:NOOK_VERSION
$installDirectory = if ($env:NOOK_INSTALL_DIR) {
    $env:NOOK_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "Programs\Nook\bin"
}

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "nook: the Windows installer currently requires x86-64 Windows"
}

$baseUrl = if ($version) {
    $version = $version.TrimStart("v")
    "https://github.com/$repository/releases/download/v$version"
} else {
    "https://github.com/$repository/releases/latest/download"
}

$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) "nook-install-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
try {
    $archivePath = Join-Path $temporaryDirectory $archive
    $checksumPath = Join-Path $temporaryDirectory $checksum
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive" -OutFile $archivePath
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$checksum" -OutFile $checksumPath

    $checksumContents = (Get-Content -Raw -LiteralPath $checksumPath).Trim()
    if ($checksumContents -notmatch "(?i)^([0-9a-f]{64})(?:\s+[* ]?.+)?$") {
        throw "nook: invalid checksum file"
    }
    $expected = $Matches[1].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "nook: release archive checksum mismatch"
    }

    $expanded = Join-Path $temporaryDirectory "expanded"
    Expand-Archive -LiteralPath $archivePath -DestinationPath $expanded
    $binary = Get-ChildItem -Path $expanded -Filter "nook.exe" -File -Recurse |
        Select-Object -First 1
    if (-not $binary) {
        throw "nook: archive does not contain nook.exe"
    }

    New-Item -ItemType Directory -Force -Path $installDirectory | Out-Null
    $installedBinary = Join-Path $installDirectory "nook.exe"
    Copy-Item -Force -LiteralPath $binary.FullName -Destination $installedBinary

    $completionDirectory = Join-Path $env:LOCALAPPDATA "Nook\completions"
    New-Item -ItemType Directory -Force -Path $completionDirectory | Out-Null
    $completionFile = Join-Path $completionDirectory "nook.ps1"
    & $installedBinary completions power-shell | Set-Content -Encoding utf8 -LiteralPath $completionFile

    $profilePath = $PROFILE.CurrentUserAllHosts
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $profilePath) | Out-Null
    $profileContents = if (Test-Path -LiteralPath $profilePath) {
        Get-Content -Raw -LiteralPath $profilePath
    } else {
        ""
    }
    $begin = "# >>> nook completions >>>"
    $end = "# <<< nook completions <<<"
    $pattern = "(?ms)^$([regex]::Escape($begin))\r?\n.*?^$([regex]::Escape($end))\r?\n?"
    $profileContents = [regex]::Replace($profileContents, $pattern, "").TrimEnd()
    $quotedCompletion = $completionFile.Replace("'", "''")
    $block = @"
$begin
if (Test-Path -LiteralPath '$quotedCompletion') { . '$quotedCompletion' }
$end
"@
    $newProfile = if ($profileContents) { "$profileContents`r`n`r`n$block`r`n" } else { "$block`r`n" }
    Set-Content -Encoding utf8 -LiteralPath $profilePath -Value $newProfile

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $pathEntries = @($userPath -split ";" | Where-Object { $_ })
    if ($pathEntries -notcontains $installDirectory) {
        [Environment]::SetEnvironmentVariable(
            "Path",
            (($pathEntries + $installDirectory) -join ";"),
            "User"
        )
        Write-Host "nook: added $installDirectory to the user PATH; open a new terminal"
    }

    Write-Host "nook: installed $installedBinary"
    Write-Host "nook: installed PowerShell completions"
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -LiteralPath $temporaryDirectory
}
