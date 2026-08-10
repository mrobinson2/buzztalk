$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) "buzztalk-gateway-test-$([Guid]::NewGuid().ToString('N'))"
$passed = 0
$failed = 0

function New-Fixture([string] $Name) {
    $fixture = Join-Path $tempRoot $Name
    $config = Join-Path $fixture 'config'
    $state = Join-Path $fixture 'state'
    New-Item -ItemType Directory -Force -Path $config, $state | Out-Null
    $key = Join-Path $fixture 'key with spaces'
    Set-Content -Encoding ascii -NoNewline -LiteralPath $key -Value 'secret-that-must-not-be-read'
    Set-Content -Encoding ascii -LiteralPath (Join-Path $config 'gateway.conf') -Value @(
        'RELAY=wss://relay.example.test/path with spaces'
        'CHANNEL=channel-id'
        'AGENT_PUBKEY=npub1-example'
        "KEY_FILE=$key"
        'HEADPHONES=true'
        'ENDPOINT_SILENCE_MS=700'
        'VPIO=false'
    )
    return [PSCustomObject]@{ Root = $fixture; Config = $config; State = $state; Key = $key }
}

function Invoke-Gateway($Fixture, [string[]] $Arguments) {
    $env:BUZZTALK_GATEWAY_CONFIG_DIR = $Fixture.Config
    $env:BUZZTALK_GATEWAY_STATE_DIR = $Fixture.State
    $env:BUZZTALKD_BIN = $script:FakeBinary
    $env:BUZZTALK_GATEWAY_STARTUP_WAIT_MS = '10'
    $env:BUZZTALK_TEST_ARGS = Join-Path $Fixture.Root 'args'
    $output = & (Join-Path $PSHOME 'pwsh.exe') -NoProfile -File (Join-Path $root 'scripts/buzztalk-gateway.ps1') @Arguments 2>&1 | Out-String
    [PSCustomObject]@{ ExitCode = $LASTEXITCODE; Output = $output }
}

function Run-Test([string] $Name, [scriptblock] $Body) {
    try { & $Body; Write-Host "ok - $Name"; $script:passed++ }
    catch { Write-Host "not ok - $Name"; Write-Host $_; $script:failed++ }
}

try {
    New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
    $fakeSource = @'
using System;
using System.IO;
using System.Threading;
public static class FakeGateway {
    public static void Main(string[] args) {
        File.WriteAllLines(Environment.GetEnvironmentVariable("BUZZTALK_TEST_ARGS"), args);
        while (true) Thread.Sleep(1000);
    }
}
'@
    $script:FakeBinary = Join-Path $tempRoot 'fake-gateway.exe'
    Add-Type -TypeDefinition $fakeSource -OutputAssembly $script:FakeBinary -OutputType ConsoleApplication

    Run-Test 'missing config gives configure action' {
        $fixture = New-Fixture 'missing'
        Remove-Item -LiteralPath (Join-Path $fixture.Config 'gateway.conf')
        $result = Invoke-Gateway $fixture @('on')
        if ($result.ExitCode -ne 2 -or $result.Output -notmatch 'Configuration not found\. Run: buzztalk-gateway\.ps1 configure') { throw $result.Output }
    }

    Run-Test 'on preserves literal arguments and does not echo sensitive values' {
        $fixture = New-Fixture 'literal'
        $result = Invoke-Gateway $fixture @('on')
        if ($result.ExitCode -ne 0) { throw $result.Output }
        $args = Get-Content -LiteralPath (Join-Path $fixture.Root 'args')
        if ($args -notcontains 'wss://relay.example.test/path with spaces') { throw 'relay argument was split' }
        if ($args -notcontains ("$($fixture.Key)")) { throw 'key path argument was split' }
        if ($result.Output -match 'relay\.example|channel-id|npub1|secret-that') { throw 'sensitive config leaked' }
        $null = Invoke-Gateway $fixture @('off')
    }

    Run-Test 'full path and creation time protect off from a changed identity' {
        $fixture = New-Fixture 'ownership'
        $result = Invoke-Gateway $fixture @('on')
        if ($result.ExitCode -ne 0) { throw $result.Output }
        $stateFile = Join-Path $fixture.State 'gateway.state'
        $state = Get-Content -Raw -LiteralPath $stateFile
        Set-Content -Encoding ascii -LiteralPath $stateFile -Value ($state -replace 'CREATION_TIME=.*', 'CREATION_TIME=1900-01-01T00:00:00.0000000Z')
        $result = Invoke-Gateway $fixture @('off')
        if ($result.ExitCode -ne 1 -or $result.Output -notmatch 'ownership could not be verified') { throw $result.Output }
        Stop-Process -Id ([int](([Select-String -InputObject $state -Pattern 'PID=(\d+)').Matches.Groups[1].Value)) -Force -ErrorAction SilentlyContinue
    }

    Run-Test 'VPIO is rejected by the Windows launcher' {
        $fixture = New-Fixture 'vpio'
        (Get-Content -LiteralPath (Join-Path $fixture.Config 'gateway.conf')) -replace '^VPIO=false$', 'VPIO=true' |
            Set-Content -Encoding ascii -LiteralPath (Join-Path $fixture.Config 'gateway.conf')
        $result = Invoke-Gateway $fixture @('on')
        if ($result.ExitCode -ne 2 -or $result.Output -notmatch 'VPIO=true is only supported on macOS') { throw $result.Output }
    }

    Write-Host "$passed passed; $failed failed"
    if ($failed -ne 0) { exit 1 }
} finally {
    foreach ($stateFile in Get-ChildItem -LiteralPath $tempRoot -Filter 'gateway.state' -Recurse -ErrorAction SilentlyContinue) {
        $pidLine = Select-String -LiteralPath $stateFile.FullName -Pattern '^PID=(\d+)$'
        if ($null -ne $pidLine) { Stop-Process -Id ([int]$pidLine.Matches[0].Groups[1].Value) -Force -ErrorAction SilentlyContinue }
    }
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -LiteralPath $tempRoot
}
