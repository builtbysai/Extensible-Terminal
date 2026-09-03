$ErrorActionPreference = 'Stop'

$marker = "EXTENSIBLE_TERMINAL_WINDOWS_SMOKE_$([Guid]::NewGuid().ToString('N'))"
$statusMarker = "EXTENSIBLE_TERMINAL_CHILD_STATUS_$([Guid]::NewGuid().ToString('N'))"
$terminal = (Resolve-Path 'target\release\terminal-cli.exe').Path
$transcript = Join-Path $env:RUNNER_TEMP 'extensible-terminal-windows-smoke.txt'
$driver = Join-Path $env:RUNNER_TEMP 'extensible-terminal-windows-smoke.sh'
$gitRoot = Join-Path $env:ProgramFiles 'Git'
$gitBash = Join-Path $gitRoot 'usr\bin\bash.exe'
$gitMintty = Join-Path $gitRoot 'usr\bin\mintty.exe'

foreach ($required in @($gitBash, $gitMintty)) {
    if (-not (Test-Path $required)) {
        Set-Content -Path $transcript -Value "required Windows smoke host tool was not found: $required"
        throw "required Windows smoke host tool was not found: $required"
    }
}

# GitHub Actions captures stdio through pipes, so terminal-cli cannot enter the
# native Windows raw-console path directly. Git for Windows ships mintty and an
# MSYS runtime that can bridge native Windows console applications through
# ConPTY. Keep the host components from the same runtime, enable that bridge
# explicitly, and let mintty retain the renderer transcript for verification.
$driverContent = @'
set -u
export MSYS=enable_pcon
# terminal-cli and cmd.exe are native Windows programs. Prevent MSYS argument
# conversion from rewriting cmd's /d and /c switches into filesystem paths.
export MSYS2_ARG_CONV_EXCL='*'
terminal="$(cygpath -u "$EXT_TERMINAL_EXE")"
"$terminal" -- cmd.exe /d /c "echo $EXT_TERMINAL_MARKER"
status=$?
printf '\r\n%s_%s\r\n' "$EXT_TERMINAL_STATUS_MARKER" "$status"
# mintty's own process status is host-specific; the transcript above carries
# the product process status explicitly and is what the harness validates.
exit 0
'@
Set-Content -Path $driver -Value $driverContent -Encoding utf8

$env:EXT_TERMINAL_EXE = $terminal
$env:EXT_TERMINAL_MARKER = $marker
$env:EXT_TERMINAL_STATUS_MARKER = $statusMarker

# mintty is a Windows GUI application, so PowerShell does not implicitly wait
# for it. ProcessStartInfo.ArgumentList preserves each argument exactly (notably
# the Git Bash path under Program Files) while WaitForExit ensures logging is
# complete before the marker assertion runs. Avoid PowerShell's read-only
# automatic $Host variable when retaining the spawned process handle.
$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $gitMintty
$startInfo.UseShellExecute = $false
foreach ($argument in @('--nodaemon', '--hold=never', '-o', 'ConPTY=true', '-s', '80,24', '-l', $transcript, $gitBash, '--noprofile', '--norc', $driver)) {
    [void]$startInfo.ArgumentList.Add($argument)
}
$hostProcess = [System.Diagnostics.Process]::Start($startInfo)
$hostProcess.WaitForExit()
$hostProcess.Dispose()

if (-not (Test-Path $transcript)) {
    throw 'Windows smoke host did not produce a renderer transcript'
}

$output = Get-Content -Raw $transcript
# Remove CSI and OSC control sequences emitted by the raw renderer before matching text.
$output = $output -replace "`e\[[0-?]*[ -/]*[@-~]", ''
$output = $output -replace "`e\][^`a]*(?:`a|`e\\)", ''

if (-not $output.Contains($marker)) {
    Write-Host '--- Windows smoke transcript ---'
    Get-Content $transcript | Out-Host
    Write-Host '--- end transcript ---'
    throw 'terminal marker did not traverse the Windows console -> terminal emulator -> renderer path'
}

if (-not $output.Contains("${statusMarker}_0")) {
    Write-Host '--- Windows smoke transcript ---'
    Get-Content $transcript | Out-Host
    Write-Host '--- end transcript ---'
    throw 'terminal-cli did not exit successfully after rendering the Windows smoke marker'
}

Write-Host 'Windows interactive terminal smoke test passed.'
