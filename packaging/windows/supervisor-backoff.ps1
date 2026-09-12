Set-StrictMode -Version Latest

function Get-CumgRestartDelay([int]$Streak,[int]$BaseDelay,[int]$MaxDelay) {
  if($BaseDelay -lt 1 -or $MaxDelay -lt $BaseDelay) { throw 'invalid restart backoff bounds' }
  if($Streak -le 1) { return $BaseDelay }
  $delay=$BaseDelay
  for($i=1; $i -lt $Streak; $i++) {
    if($delay -ge $MaxDelay) { return $MaxDelay }
    $delay=[Math]::Min($MaxDelay,$delay*2)
  }
  return [int]$delay
}
