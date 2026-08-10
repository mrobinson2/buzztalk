$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:ConfigRoot = if ($env:BUZZTALK_GATEWAY_CONFIG_DIR) { $env:BUZZTALK_GATEWAY_CONFIG_DIR } else { Join-Path $env:LOCALAPPDATA 'BuzzTalk' }
$script:StateRoot = if ($env:BUZZTALK_GATEWAY_STATE_DIR) { $env:BUZZTALK_GATEWAY_STATE_DIR } else { Join-Path $env:LOCALAPPDATA 'BuzzTalk\state' }
$script:ConfigFile = Join-Path $script:ConfigRoot 'gateway.conf'
$script:StateFile = Join-Path $script:StateRoot 'gateway.state'
$script:StdoutLog = Join-Path $script:StateRoot 'gateway.stdout.log'
$script:StderrLog = Join-Path $script:StateRoot 'gateway.stderr.log'

function Fail([string] $Message, [int] $Code = 2) {
    [Console]::Error.WriteLine($Message)
    exit $Code
}

function Validate-StartupWait {
    $value = if ($env:BUZZTALK_GATEWAY_STARTUP_WAIT_MS) { $env:BUZZTALK_GATEWAY_STARTUP_WAIT_MS } else { '1000' }
    if ($value -notmatch '^[1-9][0-9]*$') {
        Fail 'BUZZTALK_GATEWAY_STARTUP_WAIT_MS must be a positive integer.' 2
    }
    try { $script:StartupWaitMs = [int]$value } catch { Fail 'BUZZTALK_GATEWAY_STARTUP_WAIT_MS must be a positive integer.' 2 }
}

function Test-ReadableKey([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    $stream = $null
    try {
        # Open and close only; never read, parse, or display key bytes.
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        return $true
    } catch {
        return $false
    } finally {
        if ($null -ne $stream) { $stream.Dispose() }
    }
}

function Read-Configuration {
    if (-not (Test-Path -LiteralPath $script:ConfigFile -PathType Leaf)) {
        Fail 'Configuration not found. Run: buzztalk-gateway.ps1 configure' 2
    }
    $allowed = @('RELAY', 'CHANNEL', 'AGENT_PUBKEY', 'KEY_FILE', 'HEADPHONES', 'ENDPOINT_SILENCE_MS', 'VPIO')
    $seen = @{}
    $config = @{
        HEADPHONES = 'false'
        ENDPOINT_SILENCE_MS = '700'
        VPIO = 'false'
    }
    foreach ($line in Get-Content -LiteralPath $script:ConfigFile) {
        if ([string]::IsNullOrWhiteSpace($line) -or $line.StartsWith('#')) { continue }
        $separator = $line.IndexOf('=')
        if ($separator -lt 1) { Fail 'Invalid gateway configuration. Run: buzztalk-gateway.ps1 configure' 2 }
        $key = $line.Substring(0, $separator)
        $value = $line.Substring($separator + 1)
        if ($key -notin $allowed -or $seen.ContainsKey($key)) {
            Fail 'Invalid gateway configuration. Run: buzztalk-gateway.ps1 configure' 2
        }
        $seen[$key] = $true
        $config[$key] = $value
    }
    foreach ($required in @('RELAY', 'CHANNEL', 'AGENT_PUBKEY', 'KEY_FILE')) {
        if (-not $seen.ContainsKey($required) -or [string]::IsNullOrEmpty($config[$required])) {
            Fail 'Invalid gateway configuration. Run: buzztalk-gateway.ps1 configure' 2
        }
    }
    foreach ($boolean in @('HEADPHONES', 'VPIO')) {
        if ($config[$boolean] -notin @('true', 'false')) {
            Fail 'Invalid gateway configuration. Run: buzztalk-gateway.ps1 configure' 2
        }
    }
    if ($config.ENDPOINT_SILENCE_MS -notmatch '^[1-9][0-9]*$') {
        Fail 'Invalid gateway configuration. Run: buzztalk-gateway.ps1 configure' 2
    }
    if ($config.VPIO -eq 'true') {
        Fail 'VPIO=true is only supported on macOS; set VPIO=false on Windows.' 2
    }
    if (-not (Test-ReadableKey $config.KEY_FILE)) {
        Fail 'Signing key file is missing or unreadable. Check KEY_FILE and run configure again.' 2
    }
    return $config
}

function Resolve-GatewayBinary {
    $candidate = if ($env:BUZZTALKD_BIN) {
        $env:BUZZTALKD_BIN
    } else {
        Join-Path $PSScriptRoot 'buzztalkd.exe'
    }
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        $command = Get-Command buzztalkd.exe -ErrorAction SilentlyContinue
        if ($null -ne $command) { $candidate = $command.Source }
    }
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        Fail 'BuzzTalk gateway executable was not found. Reinstall BuzzTalk or set BUZZTALKD_BIN.' 1
    }
    try {
        $resolved = Resolve-Path -LiteralPath $candidate -ErrorAction Stop
        return [IO.Path]::GetFullPath($resolved.Path)
    } catch {
        Fail 'BuzzTalk gateway executable was not found. Reinstall BuzzTalk or set BUZZTALKD_BIN.' 1
    }
}

function Read-State {
    if (-not (Test-Path -LiteralPath $script:StateFile -PathType Leaf)) { return $null }
    $state = @{}
    foreach ($line in Get-Content -LiteralPath $script:StateFile) {
        $separator = $line.IndexOf('=')
        if ($separator -gt 0) { $state[$line.Substring(0, $separator)] = $line.Substring($separator + 1) }
    }
    if (-not $state.ContainsKey('PID') -or -not $state.ContainsKey('EXECUTABLE') -or -not $state.ContainsKey('CREATION_TIME')) { return $null }
    return $state
}

function Write-State([int] $Pid, [string] $Executable, [string] $CreationTime) {
    New-Item -ItemType Directory -Force -Path $script:StateRoot | Out-Null
    $temporary = Join-Path $script:StateRoot ".gateway.state-$([Guid]::NewGuid().ToString('N'))"
    @("PID=$Pid", "EXECUTABLE=$Executable", "CREATION_TIME=$CreationTime") | Set-Content -Encoding ascii -LiteralPath $temporary
    Move-Item -Force -LiteralPath $temporary -Destination $script:StateFile
}

function Remove-State { Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $script:StateFile }

function Get-ProcessIdentity([int] $Pid) {
    try {
        $process = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $Pid" -ErrorAction Stop
        if ($null -eq $process -or [string]::IsNullOrEmpty($process.ExecutablePath) -or [string]::IsNullOrEmpty($process.CreationDate)) { return $null }
        $path = [IO.Path]::GetFullPath($process.ExecutablePath)
        $creation = [Management.ManagementDateTimeConverter]::ToDateTime($process.CreationDate).ToUniversalTime().ToString('o', [Globalization.CultureInfo]::InvariantCulture)
        return [PSCustomObject]@{ ExecutablePath = $path; CreationTime = $creation }
    } catch {
        return $null
    }
}

function Test-OwnedProcess($State) {
    if ($null -eq $State) { return $false }
    try { $null = Get-Process -Id ([int]$State.PID) -ErrorAction Stop } catch { return $false }
    $identity = Get-ProcessIdentity ([int]$State.PID)
    if ($null -eq $identity) { return $false }
    $samePath = [string]::Equals($identity.ExecutablePath, [IO.Path]::GetFullPath($State.EXECUTABLE), [StringComparison]::OrdinalIgnoreCase)
    $sameCreation = [string]::Equals($identity.CreationTime, $State.CREATION_TIME, [StringComparison]::Ordinal)
    return $samePath -and $sameCreation
}

function ConvertTo-ProcessArgument([string] $Value) {
    if ($Value -notmatch '[\s"]') { return $Value }
    $builder = [Text.StringBuilder]::new('"')
    $slashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') { $slashes++; continue }
        if ($character -eq '"') {
            [void]$builder.Append(('\' * (2 * $slashes + 1)))
            [void]$builder.Append('"')
            $slashes = 0
            continue
        }
        if ($slashes -gt 0) { [void]$builder.Append(('\' * $slashes)); $slashes = 0 }
        [void]$builder.Append($character)
    }
    if ($slashes -gt 0) { [void]$builder.Append(('\' * (2 * $slashes)) ) }
    [void]$builder.Append('"')
    return $builder.ToString()
}

function Start-Gateway {
    $config = Read-Configuration
    $binary = Resolve-GatewayBinary
    $state = Read-State
    if ($null -ne $state) {
        if (Test-OwnedProcess $state) {
            Write-Output "BuzzTalk gateway is already running (PID $($state.PID))."
            return
        }
        try { $null = Get-Process -Id ([int]$state.PID) -ErrorAction Stop; Fail 'Refusing to start because recorded gateway ownership could not be verified. Run: buzztalk-gateway.ps1 status' 1 } catch [System.Management.Automation.ExitException] { throw } catch { Remove-State }
    }
    New-Item -ItemType Directory -Force -Path $script:StateRoot | Out-Null
    if (-not (Test-Path -LiteralPath $script:StdoutLog)) { New-Item -ItemType File -Path $script:StdoutLog | Out-Null }
    if (-not (Test-Path -LiteralPath $script:StderrLog)) { New-Item -ItemType File -Path $script:StderrLog | Out-Null }
    $arguments = @('--relay', $config.RELAY, '--channel', $config.CHANNEL, '--agent-pubkey', $config.AGENT_PUBKEY, '--key-file', $config.KEY_FILE, '--endpoint-silence-ms', $config.ENDPOINT_SILENCE_MS)
    if ($config.HEADPHONES -eq 'true') { $arguments += '--headphones' }
    $quoted = $arguments | ForEach-Object { ConvertTo-ProcessArgument $_ }
    $process = Start-Process -FilePath $binary -ArgumentList $quoted -RedirectStandardOutput $script:StdoutLog -RedirectStandardError $script:StderrLog -PassThru -WindowStyle Hidden
    $creation = $process.StartTime.ToUniversalTime().ToString('o', [Globalization.CultureInfo]::InvariantCulture)
    Write-State $process.Id $binary $creation
    Start-Sleep -Milliseconds $script:StartupWaitMs
    if ($process.HasExited -or -not (Test-OwnedProcess (Read-State))) {
        Remove-State
        Fail 'BuzzTalk failed during startup. Run: buzztalk-gateway.ps1 logs' 1
    }
    Write-Output "BuzzTalk gateway is running (PID $($process.Id)). Run: buzztalk-gateway.ps1 logs to view logs."
}

function Get-GatewayStatus {
    $state = Read-State
    if ($null -eq $state) { Write-Output 'BuzzTalk gateway is stopped.'; return 3 }
    if (Test-OwnedProcess $state) { Write-Output "BuzzTalk gateway is running (PID $($state.PID))."; return 0 }
    try { $null = Get-Process -Id ([int]$state.PID) -ErrorAction Stop; [Console]::Error.WriteLine("Refusing to report PID $($state.PID) as owned. Run: buzztalk-gateway.ps1 status"); return 1 } catch { Remove-State; Write-Output 'BuzzTalk gateway has stale state and is stopped. Run: buzztalk-gateway.ps1 on'; return 3 }
}

function Stop-Gateway {
    $state = Read-State
    if ($null -eq $state) { Write-Output 'BuzzTalk gateway is already stopped.'; return }
    if (-not (Test-OwnedProcess $state)) {
        try { $null = Get-Process -Id ([int]$state.PID) -ErrorAction Stop; Fail "Refusing to stop PID $($state.PID) because ownership could not be verified. Run: buzztalk-gateway.ps1 status" 1 } catch [System.Management.Automation.ExitException] { throw } catch { Remove-State; Write-Output 'BuzzTalk gateway is stopped.'; return }
    }
    Stop-Process -Id ([int]$state.PID) -ErrorAction SilentlyContinue
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ([DateTime]::UtcNow -lt $deadline) {
        try { $null = Get-Process -Id ([int]$state.PID) -ErrorAction Stop; Start-Sleep -Milliseconds 100 } catch { break }
    }
    try { $null = Get-Process -Id ([int]$state.PID) -ErrorAction Stop; Stop-Process -Id ([int]$state.PID) -Force -ErrorAction SilentlyContinue } catch { }
    Remove-State
    Write-Output 'BuzzTalk gateway is stopped.'
}

function Configure-Gateway {
    if (Test-Path -LiteralPath $script:ConfigFile -PathType Leaf) {
        $answer = Read-Host 'Configuration already exists. Replace it? [y/N]'
        if ($answer -notin @('y', 'Y')) { Write-Output 'Configuration unchanged.'; return }
    } elseif ([Console]::IsInputRedirected) {
        Fail 'Configuration not found. Run configure interactively.' 2
    }
    $relay = Read-Host 'Relay URL'
    $channel = Read-Host 'Channel UUID'
    $agent = Read-Host 'Agent public key'
    $key = Read-Host 'Signing-key file path'
    if (-not (Test-ReadableKey $key)) { Fail 'Signing key file is missing or unreadable. Check KEY_FILE and run configure again.' 2 }
    $headphones = if ((Read-Host 'Use headphones? [y/N]') -in @('y', 'Y')) { 'true' } else { 'false' }
    $silence = Read-Host 'Endpoint silence in ms [700]'; if ([string]::IsNullOrEmpty($silence)) { $silence = '700' }
    if ($silence -notmatch '^[1-9][0-9]*$') { Fail 'Invalid gateway configuration. Run: buzztalk-gateway.ps1 configure' 2 }
    New-Item -ItemType Directory -Force -Path $script:ConfigRoot | Out-Null
    $temporary = Join-Path $script:ConfigRoot ".gateway.conf-$([Guid]::NewGuid().ToString('N'))"
    @("RELAY=$relay", "CHANNEL=$channel", "AGENT_PUBKEY=$agent", "KEY_FILE=$key", "HEADPHONES=$headphones", "ENDPOINT_SILENCE_MS=$silence", 'VPIO=false') | Set-Content -Encoding ascii -LiteralPath $temporary
    Move-Item -Force -LiteralPath $temporary -Destination $script:ConfigFile
    Write-Output 'Configuration saved. Run: buzztalk-gateway.ps1 on'
}

Validate-StartupWait
$command = if ($args.Count -gt 0) { $args[0] } else { '' }
switch ($command) {
    'configure' { Configure-Gateway }
    'on' { Start-Gateway }
    'off' { Stop-Gateway }
    'status' { exit (Get-GatewayStatus) }
    'toggle' { if ((Get-GatewayStatus) -eq 0) { Stop-Gateway } else { Start-Gateway } }
    'logs' {
        $paths = @($script:StdoutLog, $script:StderrLog) | Where-Object { Test-Path -LiteralPath $_ }
        if ($paths.Count -eq 0) { Fail 'Gateway log is not available. Run: buzztalk-gateway.ps1 on' 1 }
        Get-Content -Wait -Tail 50 -LiteralPath $paths
    }
    { $_ -in @('help', '-h', '--help') } { Write-Output 'Usage: buzztalk-gateway.ps1 <configure|on|off|status|toggle|logs>' }
    default { Write-Output 'Usage: buzztalk-gateway.ps1 <configure|on|off|status|toggle|logs>'; exit 2 }
}
