$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Fail([string] $Message) {
    [Console]::Error.WriteLine("buzztalk installer: $Message")
    exit 1
}

if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
)) {
    Fail "unsupported platform: $([System.Runtime.InteropServices.RuntimeInformation]::OSDescription)"
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($architecture -ne 'X64') {
    Fail "unsupported platform: Windows $architecture"
}

$version = if ($env:BUZZTALK_VERSION) {
    $env:BUZZTALK_VERSION
} else {
    $releaseApi = if ($env:BUZZTALK_RELEASE_API_URL) {
        $env:BUZZTALK_RELEASE_API_URL
    } else {
        'https://api.github.com/repos/mrobinson2/buzztalk/releases?per_page=10'
    }
    $releases = Invoke-RestMethod -Uri $releaseApi
    $release = $null
    foreach ($candidate in $releases) {
        if (-not $candidate.draft) {
            $release = $candidate
            break
        }
    }
    if ($null -eq $release -or -not $release.tag_name) {
        Fail 'GitHub releases response contained no non-draft release tag'
    }
    $release.tag_name
}
$installDir = if ($env:BUZZTALK_INSTALL_DIR) {
    $env:BUZZTALK_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'BuzzTalk\bin'
}
$releaseBase = if ($env:BUZZTALK_RELEASE_BASE_URL) {
    $env:BUZZTALK_RELEASE_BASE_URL.TrimEnd('/')
} else {
    'https://github.com/mrobinson2/buzztalk/releases'
}
$releaseUrl = "$releaseBase/download/$version"
$archiveName = 'buzztalk-windows-x86_64.zip'

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$stageDir = Join-Path $installDir ".buzztalk-install-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $stageDir | Out-Null

try {
    $manifestPath = Join-Path $stageDir 'SHA256SUMS'
    $archivePath = Join-Path $stageDir $archiveName
    $combinedManifest = $true
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/SHA256SUMS" -OutFile $manifestPath
    } catch {
        $combinedManifest = $false
        Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/buzztalk-windows-x86_64.sha256" -OutFile $manifestPath
    }
    Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/$archiveName" -OutFile $archivePath

    $escapedName = [Regex]::Escape($archiveName)
    $manifest = Get-Content -Raw $manifestPath
    $checksumPattern = if ($combinedManifest) {
        "(?im)^([0-9a-f]{64})\s+\*?$escapedName\s*$"
    } else {
        "(?im)^([0-9a-f]{64})(?:\s+\*?$escapedName)?\s*$"
    }
    $match = [Regex]::Match($manifest, $checksumPattern)
    if (-not $match.Success) {
        Fail "checksum manifest has no SHA-256 for $archiveName"
    }
    $expected = $match.Groups[1].Value.ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        Fail "checksum verification failed for $archiveName"
    }

    $payload = Join-Path $stageDir 'payload'
    Expand-Archive -Path $archivePath -DestinationPath $payload
    $daemon = Join-Path $payload 'buzztalkd.exe'
    $demo = Join-Path $payload 'buzztalk-demo.exe'
    if (-not (Test-Path -PathType Leaf $daemon)) { Fail 'release archive is missing buzztalkd.exe' }
    if (-not (Test-Path -PathType Leaf $demo)) { Fail 'release archive is missing buzztalk-demo.exe' }

    # Move-Item on the same volume replaces each executable by rename. Keep
    # recoverable copies until the complete pair is installed so a failure
    # cannot leave binaries from two different releases.
    $destinations = @{
        'buzztalkd.exe' = Join-Path $installDir 'buzztalkd.exe'
        'buzztalk-demo.exe' = Join-Path $installDir 'buzztalk-demo.exe'
    }
    $sources = @{
        'buzztalkd.exe' = $daemon
        'buzztalk-demo.exe' = $demo
    }
    $backup = Join-Path $stageDir 'backup'
    New-Item -ItemType Directory -Path $backup | Out-Null
    foreach ($binary in $destinations.Keys) {
        if (Test-Path -PathType Leaf $destinations[$binary]) {
            Copy-Item -Path $destinations[$binary] -Destination (Join-Path $backup $binary)
        }
    }

    $transactionStarted = $true
    $committed = $false
    try {
        foreach ($binary in @('buzztalkd.exe', 'buzztalk-demo.exe')) {
            Move-Item -Force -Path $sources[$binary] -Destination $destinations[$binary]
        }
        $committed = $true
    } catch {
        $replacementError = $_.Exception.Message
        throw "could not replace the complete executable set; the previous installation was restored: $replacementError"
    } finally {
        if ($transactionStarted -and -not $committed) {
            foreach ($binary in @('buzztalkd.exe', 'buzztalk-demo.exe')) {
                # A same-volume rename removes the staged source. If it is
                # still present, that executable was never replaced.
                if (Test-Path -PathType Leaf $sources[$binary]) {
                    continue
                }
                $backupPath = Join-Path $backup $binary
                if (Test-Path -PathType Leaf $backupPath) {
                    Move-Item -Force -Path $backupPath -Destination $destinations[$binary]
                } else {
                    Remove-Item -Force -ErrorAction SilentlyContinue $destinations[$binary]
                }
            }
        }
    }

    Write-Host "Installed BuzzTalk $version to $installDir"
    Write-Host 'Speech models were not downloaded. Model download remains opt-in via buzztalkd --download-models.'
} catch {
    Fail $_.Exception.Message
} finally {
    if (Test-Path $stageDir) {
        Remove-Item -Recurse -Force $stageDir
    }
}
