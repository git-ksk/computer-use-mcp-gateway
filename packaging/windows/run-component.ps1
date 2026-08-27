[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$ConfigPath)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'

function Log([string]$m) {
  Add-Content -LiteralPath $script:SupervisorLog -Encoding UTF8 -Value "$([DateTimeOffset]::UtcNow.ToString('o')) $m"
}

function Quote-Arg([AllowEmptyString()][string]$v) {
  if ($v.Length -eq 0) { return '""' }
  if ($v -notmatch '[\s"]') { return $v }
  $b=New-Object Text.StringBuilder; [void]$b.Append('"'); $slashes=0
  foreach($c in $v.ToCharArray()) {
    if($c -eq '\') { $slashes++; continue }
    if($c -eq '"') { [void]$b.Append(('\'*(($slashes*2)+1))); [void]$b.Append('"'); $slashes=0; continue }
    if($slashes) { [void]$b.Append(('\'*$slashes)); $slashes=0 }
    [void]$b.Append($c)
  }
  if($slashes) { [void]$b.Append(('\'*($slashes*2))) }
  [void]$b.Append('"'); $b.ToString()
}

function Test-Loopback([string]$endpoint) {
  if($endpoint -notmatch '^127\.0\.0\.1:(\d{1,5})$') { throw 'waitForTcp must be 127.0.0.1:PORT' }
  $port=[int]$Matches[1]; if($port -lt 1 -or $port -gt 65535) { throw 'invalid waitForTcp port' }
  $c=New-Object Net.Sockets.TcpClient
  try { $a=$c.BeginConnect('127.0.0.1',$port,$null,$null); if(-not $a.AsyncWaitHandle.WaitOne(1000)){return $false}; $c.EndConnect($a); return $true }
  catch { return $false }
  finally { $c.Dispose() }
}

$config=Get-Content -LiteralPath (Resolve-Path -LiteralPath $ConfigPath) -Raw -Encoding UTF8|ConvertFrom-Json
foreach($n in 'component','executable','workingDirectory','logDirectory') {
  if(-not $config.PSObject.Properties.Name.Contains($n) -or [string]::IsNullOrWhiteSpace([string]$config.$n)) { throw "missing config field: $n" }
}
$component=[string]$config.component
if($component -notmatch '^[A-Za-z0-9._-]+$'){throw 'invalid component'}
$exe=[Environment]::ExpandEnvironmentVariables([string]$config.executable); $work=[Environment]::ExpandEnvironmentVariables([string]$config.workingDirectory); $logs=[Environment]::ExpandEnvironmentVariables([string]$config.logDirectory)
foreach($p in $exe,$work,$logs){if(-not [IO.Path]::IsPathRooted($p)){throw 'all paths must be absolute'}}
if(-not(Test-Path -LiteralPath $exe -PathType Leaf)){throw "missing executable: $exe"}
if(-not(Test-Path -LiteralPath $work -PathType Container)){throw "missing working directory: $work"}
New-Item -ItemType Directory -Path $logs -Force|Out-Null
$script:SupervisorLog=Join-Path $logs "$component.supervisor.log"
$archive=Join-Path $logs 'archive'; New-Item -ItemType Directory -Path $archive -Force|Out-Null
$pidFile=if($config.PSObject.Properties.Name.Contains('pidFile') -and -not [string]::IsNullOrWhiteSpace([string]$config.pidFile)){[Environment]::ExpandEnvironmentVariables([string]$config.pidFile)}else{Join-Path $logs "$component.pid"}
if(-not [IO.Path]::IsPathRooted($pidFile)){throw 'pidFile must be absolute'}
$restart=if($config.PSObject.Properties.Name.Contains('restartDelaySeconds')){[int]$config.restartDelaySeconds}else{2}
$startup=if($config.PSObject.Properties.Name.Contains('startupDelaySeconds')){[int]$config.startupDelaySeconds}else{0}
if($restart -lt 1 -or $restart -gt 300){throw 'restartDelaySeconds must be 1..300'}
if($startup -lt 0 -or $startup -gt 300){throw 'startupDelaySeconds must be 0..300'}
$wait=if($config.PSObject.Properties.Name.Contains('waitForTcp')){[string]$config.waitForTcp}else{''}
$args=@(); if($config.PSObject.Properties.Name.Contains('arguments') -and $null -ne $config.arguments){$args=@($config.arguments|ForEach-Object{[Environment]::ExpandEnvironmentVariables([string]$_)})}
$argLine=($args|ForEach-Object{Quote-Arg $_}) -join ' '
if($startup){Log "component=$component event=startup_delay seconds=$startup"; Start-Sleep -Seconds $startup}
$waiting=$false
while($true){
  try {
    if(-not [string]::IsNullOrWhiteSpace($wait)){
      while(-not(Test-Loopback $wait)){
        if(-not $waiting){Log "component=$component event=dependency_wait endpoint=$wait"; $waiting=$true}
        Start-Sleep -Seconds 1
      }
      if($waiting){Log "component=$component event=dependency_ready endpoint=$wait"; $waiting=$false}
    }
    $runStamp=[DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ')
    $stdout=Join-Path $archive "$component.$runStamp.stdout.log"
    $stderr=Join-Path $archive "$component.$runStamp.stderr.log"
    $sp=@{FilePath=$exe;WorkingDirectory=$work;RedirectStandardOutput=$stdout;RedirectStandardError=$stderr;PassThru=$true;WindowStyle='Hidden'}
    if(-not [string]::IsNullOrWhiteSpace($argLine)){$sp.ArgumentList=$argLine}
    $p=Start-Process @sp
    $childPid=$p.Id
    Set-Content -LiteralPath $pidFile -Value $childPid -Encoding ASCII
    Log "component=$component event=child_start pid=$childPid"
    try {
      $p.WaitForExit(); $p.Refresh(); $code=$p.ExitCode; if($null -eq $code){$code='unavailable'}
    } finally {
      $p.Dispose()
    }
    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    Log "component=$component event=child_exit pid=$childPid exit_code=$code restart_in_seconds=$restart"
  } catch {
    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    Log "component=$component event=supervisor_error type=$($_.Exception.GetType().Name) restart_in_seconds=$restart"
  }
  Start-Sleep -Seconds $restart
}
