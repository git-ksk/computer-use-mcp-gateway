[CmdletBinding(SupportsShouldProcess=$true)]
param(
 [Parameter(Mandatory=$true)][string]$DataRoot,
 [Parameter(Mandatory=$true)][string]$HubConfig,
 [Parameter(Mandatory=$true)][string]$AgentConfig,
 [string]$ProxyConfig,
 [string]$TaskPrefix='cumg-v2-windows',
 [switch]$Start
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'

function Required-File([string]$p,[string]$name){if(-not(Test-Path -LiteralPath $p -PathType Leaf)){throw "$name missing: $p"};(Resolve-Path -LiteralPath $p).Path}
function Protect-Tree([string]$root){
 $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
 function Apply-DirAcl([string]$path){& icacls.exe $path '/inheritance:r' '/grant:r' "*$($sid):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' '/Q'|Out-Null; if($LASTEXITCODE -ne 0){throw "directory ACL failed: $path"}}
 function Apply-FileAcl([string]$path){& icacls.exe $path '/inheritance:r' '/grant:r' "*$($sid):F" '*S-1-5-18:F' '*S-1-5-32-544:F' '/Q'|Out-Null; if($LASTEXITCODE -ne 0){throw "file ACL failed: $path"}}
 Apply-DirAcl $root
 Get-ChildItem -LiteralPath $root -Recurse -Force -Directory|ForEach-Object{Apply-DirAcl $_.FullName}
 Get-ChildItem -LiteralPath $root -Recurse -Force -File|ForEach-Object{Apply-FileAcl $_.FullName}
}
function Register-Component([string]$name,[string]$config,[int]$delay){
 $launcher=Join-Path $PSScriptRoot 'run-component.ps1'; $ps="$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
 $action=New-ScheduledTaskAction -Execute $ps -Argument "-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$launcher`" -ConfigPath `"$config`"" -WorkingDirectory $PSScriptRoot
 $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
 $principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited
 $trigger=New-ScheduledTaskTrigger -AtLogOn -User $sid; if($delay){$trigger.Delay="PT${delay}S"}
 $settings=New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
 if($PSCmdlet.ShouldProcess($name,'register CUMG V2 limited per-user persistence task')){Register-ScheduledTask -TaskName $name -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force|Out-Null}
}
$root=(Resolve-Path -LiteralPath $DataRoot).Path; $hub=Required-File $HubConfig 'HubConfig'; $agent=Required-File $AgentConfig 'AgentConfig'; $proxy=$null
if(-not [string]::IsNullOrWhiteSpace($ProxyConfig)){$proxy=Required-File $ProxyConfig 'ProxyConfig'}
if($PSCmdlet.ShouldProcess($root,'protect tree for current identity, SYSTEM, Administrators')){Protect-Tree $root}
Register-Component "$TaskPrefix-hub" $hub 0; Register-Component "$TaskPrefix-agent" $agent 2; if($null -ne $proxy){Register-Component "$TaskPrefix-proxy" $proxy 4}
if($Start){foreach($s in 'hub','agent','proxy'){$n="$TaskPrefix-$s"; if(Get-ScheduledTask -TaskName $n -ErrorAction SilentlyContinue){if($PSCmdlet.ShouldProcess($n,'start task')){Start-ScheduledTask -TaskName $n}}}}
