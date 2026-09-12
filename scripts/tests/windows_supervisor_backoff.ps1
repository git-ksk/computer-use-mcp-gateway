$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
. (Join-Path $repo 'packaging\windows\supervisor-backoff.ps1')

$delays = @(1..6 | ForEach-Object { Get-CumgRestartDelay -Streak $_ -BaseDelay 1 -MaxDelay 4 })
if(($delays -join ',') -ne '1,2,4,4,4,4') {
  throw "unexpected restart backoff: $($delays -join ',')"
}
if((Get-CumgRestartDelay -Streak 0 -BaseDelay 2 -MaxDelay 60) -ne 2) {
  throw 'stable/reset streak must return the base delay'
}
$invalid = $false
try { Get-CumgRestartDelay -Streak 2 -BaseDelay 4 -MaxDelay 2 | Out-Null } catch { $invalid = $true }
if(-not $invalid) { throw 'invalid backoff bounds must fail closed' }
Write-Host "Windows supervisor backoff policy verified: delays=$($delays -join ',')"
