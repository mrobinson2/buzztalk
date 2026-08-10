$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$PSNativeCommandUseErrorActionPreference = $false

$root = Split-Path -Parent $PSScriptRoot
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) "buzztalk-install-test-$([Guid]::NewGuid().ToString('N'))"
$port = Get-Random -Minimum 20000 -Maximum 45000
$server = $null
$passed = 0
$failed = 0

function New-FakeRelease([string] $Version, [string] $Marker, [bool] $BadChecksum = $false) {
    $releaseDir = Join-Path $tempRoot "releases\download\$Version"
    $payload = Join-Path $tempRoot "payload-$Marker"
    New-Item -ItemType Directory -Force -Path $releaseDir, $payload | Out-Null
    Set-Content -Encoding ascii -NoNewline -Path (Join-Path $payload 'buzztalkd.exe') -Value "$Marker-daemon"
    Set-Content -Encoding ascii -NoNewline -Path (Join-Path $payload 'buzztalk-demo.exe') -Value "$Marker-demo"
    $archiveName = 'buzztalk-windows-x86_64.zip'
    $archivePath = Join-Path $releaseDir $archiveName
    $payloadFiles = @(
        (Join-Path $payload 'buzztalkd.exe')
        (Join-Path $payload 'buzztalk-demo.exe')
    )
    Compress-Archive -Path $payloadFiles -DestinationPath $archivePath
    $hash = if ($BadChecksum) { '0' * 64 } else { (Get-FileHash -Algorithm SHA256 $archivePath).Hash.ToLowerInvariant() }
    $legacyChecksum = $Marker -in @('prerelease', 'corrupt')
    $checksumName = if ($legacyChecksum) { 'buzztalk-windows-x86_64.sha256' } else { 'SHA256SUMS' }
    $checksumValue = if ($legacyChecksum) { $hash } else { "$hash  $archiveName" }
    Set-Content -Encoding ascii -Path (Join-Path $releaseDir $checksumName) -Value $checksumValue
}

function Invoke-Installer([AllowNull()][string] $Version, [string] $InstallDir) {
    if ($null -eq $Version) {
        Remove-Item Env:BUZZTALK_VERSION -ErrorAction SilentlyContinue
    } else {
        $env:BUZZTALK_VERSION = $Version
    }
    $env:BUZZTALK_INSTALL_DIR = $InstallDir
    $env:BUZZTALK_RELEASE_BASE_URL = "http://127.0.0.1:$port/releases"
    $env:BUZZTALK_RELEASE_API_URL = "http://127.0.0.1:$port/releases.json"
    $output = & (Join-Path $PSHOME 'pwsh.exe') -NoProfile -File (Join-Path $root 'install.ps1') 2>&1 | Out-String
    [PSCustomObject]@{ ExitCode = $LASTEXITCODE; Output = $output }
}

function Invoke-InterruptedInstaller([string] $Version, [string] $InstallDir) {
    $env:BUZZTALK_VERSION = $Version
    $env:BUZZTALK_INSTALL_DIR = $InstallDir
    $env:BUZZTALK_RELEASE_BASE_URL = "http://127.0.0.1:$port/releases"
    $env:BUZZTALK_RELEASE_API_URL = "http://127.0.0.1:$port/releases.json"
    $harness = Join-Path $tempRoot 'interrupt-installer.ps1'
    Set-Content -Encoding utf8 -Path $harness -Value @'
param([string] $Installer)
$script:moveCount = 0
function Move-Item {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)] [string[]] $Path,
        [Parameter(Mandatory = $true)] [string] $Destination,
        [switch] $Force
    )
    $parameters = @{ Path = $Path; Destination = $Destination }
    if ($Force) { $parameters.Force = $true }
    Microsoft.PowerShell.Management\Move-Item @parameters
    $script:moveCount++
    if ($script:moveCount -eq 1) {
        throw [System.Management.Automation.PipelineStoppedException]::new()
    }
}
& $Installer
exit $LASTEXITCODE
'@
    $output = & (Join-Path $PSHOME 'pwsh.exe') -NoProfile -File $harness (Join-Path $root 'install.ps1') 2>&1 | Out-String
    [PSCustomObject]@{ ExitCode = $LASTEXITCODE; Output = $output }
}

function Run-Test([string] $Name, [scriptblock] $Body) {
    try {
        & $Body
        Write-Host "ok - $Name"
        $script:passed++
    } catch {
        Write-Host "not ok - $Name"
        Write-Host $_
        $script:failed++
    }
}

try {
    New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
    New-FakeRelease -Version 'v9.9.0-rc.1' -Marker 'prerelease'
    New-FakeRelease -Version 'v9.9.1-bad' -Marker 'corrupt' -BadChecksum $true
    New-FakeRelease -Version 'v9.9.2' -Marker 'selected'
    Set-Content -Encoding utf8 -Path (Join-Path $tempRoot 'releases.json') -Value @'
[
  {
    "tag_name": "v10.0.0-draft",
    "draft": true,
    "prerelease": false
  },
  {
    "tag_name": "v9.9.0-rc.1",
    "draft": false,
    "prerelease": true
  }
]
'@

    $python = (Get-Command python).Source
    $server = Start-Process -FilePath $python -ArgumentList '-m', 'http.server', $port, '--bind', '127.0.0.1' -WorkingDirectory $tempRoot -PassThru -WindowStyle Hidden
    $ready = $false
    foreach ($attempt in 1..30) {
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$port/releases.json" | Out-Null
            $ready = $true
            break
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $ready) { throw 'fake release server did not start' }

    Run-Test 'selected version installs both binaries and reruns safely' {
        $installDir = Join-Path $tempRoot 'selected'
        $result = Invoke-Installer -Version 'v9.9.2' -InstallDir $installDir
        if ($result.ExitCode -ne 0) { throw "installer failed: $($result.Output)" }
        if ((Get-Content -Raw (Join-Path $installDir 'buzztalkd.exe')) -ne 'selected-daemon') { throw 'wrong daemon installed' }
        if ((Get-Content -Raw (Join-Path $installDir 'buzztalk-demo.exe')) -ne 'selected-demo') { throw 'wrong demo installed' }
        if (Test-Path (Join-Path $installDir 'models')) { throw 'installer downloaded models' }

        $rerun = Invoke-Installer -Version 'v9.9.2' -InstallDir $installDir
        if ($rerun.ExitCode -ne 0) { throw "installer rerun failed: $($rerun.Output)" }
        if ((Get-Content -Raw (Join-Path $installDir 'buzztalkd.exe')) -ne 'selected-daemon') { throw 'rerun changed daemon content' }
    }

    Run-Test 'unset version installs prerelease with per-archive checksum' {
        $installDir = Join-Path $tempRoot 'installed'
        $result = Invoke-Installer -Version $null -InstallDir $installDir
        if ($result.ExitCode -ne 0) { throw "installer failed: $($result.Output)" }
        if ((Get-Content -Raw (Join-Path $installDir 'buzztalkd.exe')) -ne 'prerelease-daemon') { throw 'wrong daemon installed' }
        if ((Get-Content -Raw (Join-Path $installDir 'buzztalk-demo.exe')) -ne 'prerelease-demo') { throw 'wrong demo installed' }
    }

    Run-Test 'checksum failure preserves existing binaries' {
        $installDir = Join-Path $tempRoot 'existing'
        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        Set-Content -Encoding ascii -NoNewline -Path (Join-Path $installDir 'buzztalkd.exe') -Value 'old-daemon'
        Set-Content -Encoding ascii -NoNewline -Path (Join-Path $installDir 'buzztalk-demo.exe') -Value 'old-demo'
        $result = Invoke-Installer -Version 'v9.9.1-bad' -InstallDir $installDir
        if ($result.ExitCode -eq 0) { throw 'installer accepted a bad checksum' }
        if ($result.Output -notmatch 'checksum verification failed') { throw "missing checksum diagnostic: $($result.Output)" }
        if ((Get-Content -Raw (Join-Path $installDir 'buzztalkd.exe')) -ne 'old-daemon') { throw 'daemon was replaced after checksum failure' }
        if ((Get-Content -Raw (Join-Path $installDir 'buzztalk-demo.exe')) -ne 'old-demo') { throw 'demo was replaced after checksum failure' }
    }

    Run-Test 'second replacement failure rolls back the first binary' {
        $installDir = Join-Path $tempRoot 'rollback'
        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $daemonPath = Join-Path $installDir 'buzztalkd.exe'
        $demoPath = Join-Path $installDir 'buzztalk-demo.exe'
        Set-Content -Encoding ascii -NoNewline -Path $daemonPath -Value 'old-daemon'
        Set-Content -Encoding ascii -NoNewline -Path $demoPath -Value 'old-demo'

        $lockedDemo = [IO.File]::Open(
            $demoPath,
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
        try {
            $result = Invoke-Installer -Version 'v9.9.2' -InstallDir $installDir
        } finally {
            $lockedDemo.Dispose()
        }

        if ($result.ExitCode -eq 0) { throw 'installer accepted a partial replacement' }
        if ($result.Output -notmatch 'previous installation was restored') {
            throw "missing rollback diagnostic: $($result.Output)"
        }
        if ((Get-Content -Raw $daemonPath) -ne 'old-daemon') { throw 'daemon was not rolled back' }
        if ((Get-Content -Raw $demoPath) -ne 'old-demo') { throw 'demo changed despite replacement failure' }
    }

    Run-Test 'pipeline interruption after first replacement restores both binaries' {
        $installDir = Join-Path $tempRoot 'interrupted'
        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $daemonPath = Join-Path $installDir 'buzztalkd.exe'
        $demoPath = Join-Path $installDir 'buzztalk-demo.exe'
        Set-Content -Encoding ascii -NoNewline -Path $daemonPath -Value 'old-daemon'
        Set-Content -Encoding ascii -NoNewline -Path $demoPath -Value 'old-demo'

        $result = Invoke-InterruptedInstaller -Version 'v9.9.2' -InstallDir $installDir

        if ($result.ExitCode -eq 0) { throw 'installer ignored a pipeline interruption' }
        if ((Get-Content -Raw $daemonPath) -ne 'old-daemon') { throw 'daemon was not restored after interruption' }
        if ((Get-Content -Raw $demoPath) -ne 'old-demo') { throw 'demo was not restored after interruption' }
    }

    Write-Host "$passed passed; $failed failed"
    if ($failed -ne 0) { exit 1 }
    exit 0
} finally {
    if ($null -ne $server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force }
    if (Test-Path $tempRoot) { Remove-Item -Recurse -Force $tempRoot }
}
