# =============================================================================
# Portal suggest (autocomplete) acceptance probe -- local PowerShell entry.
#
# Uploads verify-portal-suggest.sh to the server and runs it there.
# See that file for what the probe checks and how to read its verdict.
#
# Read-only end to end: SELECTs feishu_datasource_portal + feishu_option,
# and calls the suggest endpoint only. No data is written.
#
# Usage (from the project root, i.e. the parent of deploy/):
#   .\deploy\verify-portal-suggest.ps1
#
# WHY THIS FILE IS PURE ASCII: Windows PowerShell 5.1 reads a BOM-less .ps1 as
# ANSI/GBK. Non-ASCII here would be mangled into bytes that break the parser.
# All Chinese lives in the .sh, which bash reads as bytes and which scp copies
# verbatim.
#
# The .sh is normalized to LF on the way out: a CRLF `set -uo pipefail` would
# make bash fail on the trailing carriage return.
# =============================================================================
param(
    [string]$SshUser   = 'yjj',
    [string]$SshHost   = '47.109.148.207',
    [string]$SshKey    = 'C:\Users\16040\.ssh\yjj_47.109.148.207',
    [string]$RemoteDir = '/home/yjj/yang-system'
)

$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$localSh   = Join-Path $scriptDir 'verify-portal-suggest.sh'
if (-not (Test-Path $localSh)) { throw "missing probe script: $localSh" }

$remoteSh = Split-Path -Leaf $localSh

# LF-normalize into a temp copy; UTF-8 (no BOM) so the Chinese survives.
$tempSh = Join-Path $env:TEMP $remoteSh
$content = [System.IO.File]::ReadAllText($localSh) -replace "`r`n", "`n"
[System.IO.File]::WriteAllText($tempSh, $content, (New-Object System.Text.UTF8Encoding $false))

$sshTarget = "$SshUser@$SshHost"
$sshOpts   = @('-o', 'BatchMode=yes', '-o', 'ConnectTimeout=15')

try {
    Write-Host "==> upload probe to ${sshTarget}:${RemoteDir}" -ForegroundColor Cyan
    & scp @sshOpts -i $SshKey $tempSh "${sshTarget}:${RemoteDir}/"
    if ($LASTEXITCODE -ne 0) { throw "scp failed (exit $LASTEXITCODE)" }

    Write-Host "==> run probe on the server" -ForegroundColor Cyan
    $remoteCmd = "cd $RemoteDir && bash $remoteSh"
    & ssh @sshOpts -i $SshKey $sshTarget $remoteCmd
    $exit = $LASTEXITCODE
}
finally {
    Remove-Item -LiteralPath $tempSh -ErrorAction SilentlyContinue
}

if ($exit -ne 0) {
    Write-Host "`nprobe exited $exit (see the verdict above)." -ForegroundColor Yellow
} else {
    Write-Host "`nprobe finished." -ForegroundColor Green
}
exit $exit
