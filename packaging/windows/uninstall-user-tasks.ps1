[CmdletBinding(SupportsShouldProcess=$true)]
param([string]$TaskPrefix='cumg-v2-windows',[string[]]$ConfigPaths=@(),[switch]$DisableOnly)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'

# Stop the persistence launchers first. Killing a supervised child before its launcher
# can race with run-component.ps1's restart loop and leave a replacement child orphaned.
foreach($s in 'proxy','agent','hub'){
 $n="$TaskPrefix-$s"; $t=Get-ScheduledTask -TaskName $n -ErrorAction SilentlyContinue; if($null -eq $t){continue}
 if($PSCmdlet.ShouldProcess($n,'stop task launcher')){Stop-ScheduledTask -TaskName $n -ErrorAction SilentlyContinue}
}

# Once the launcher is stopped, terminate any child recorded in its PID file.
foreach($cp in $ConfigPaths){
 if(-not(Test-Path -LiteralPath $cp -PathType Leaf)){continue}
 try{
  $c=Get-Content -LiteralPath $cp -Raw -Encoding UTF8|ConvertFrom-Json
  $pf=if($c.PSObject.Properties.Name.Contains('pidFile') -and -not [string]::IsNullOrWhiteSpace([string]$c.pidFile)){[Environment]::ExpandEnvironmentVariables([string]$c.pidFile)}else{Join-Path ([Environment]::ExpandEnvironmentVariables([string]$c.logDirectory)) "$([string]$c.component).pid"}
  if(Test-Path -LiteralPath $pf -PathType Leaf){
   $id=[int](Get-Content -LiteralPath $pf -Raw)
   $proc=Get-Process -Id $id -ErrorAction SilentlyContinue
   if($null -ne $proc -and $PSCmdlet.ShouldProcess("PID $id","stop CUMG child $($proc.ProcessName)")){Stop-Process -Id $id -Force}
   Remove-Item -LiteralPath $pf -Force -ErrorAction SilentlyContinue
  }
 }catch{Write-Warning "could not stop child described by $cp"}
}

foreach($s in 'proxy','agent','hub'){
 $n="$TaskPrefix-$s"; $t=Get-ScheduledTask -TaskName $n -ErrorAction SilentlyContinue; if($null -eq $t){continue}
 if($DisableOnly){if($PSCmdlet.ShouldProcess($n,'disable task')){Disable-ScheduledTask -TaskName $n|Out-Null}}
 else{if($PSCmdlet.ShouldProcess($n,'unregister task')){Unregister-ScheduledTask -TaskName $n -Confirm:$false}}
}
