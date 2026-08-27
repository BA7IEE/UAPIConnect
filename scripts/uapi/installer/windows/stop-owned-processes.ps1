[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$InstallDir
)

$ErrorActionPreference = "Stop"
$comparison = [System.StringComparison]::OrdinalIgnoreCase
$processNames = @(
  "codex-plus-plus.exe",
  "codex-plus-plus-manager.exe"
)

$normalizedInstallDir = [System.IO.Path]::GetFullPath($InstallDir)
if ([string]::IsNullOrWhiteSpace($normalizedInstallDir)) {
  throw "InstallDir does not resolve to a usable path"
}

$expectedPaths = @{}
foreach ($processName in $processNames) {
  $expectedPaths[$processName] = [System.IO.Path]::GetFullPath(
    (Join-Path $normalizedInstallDir $processName)
  )
}

function Get-ProcessOwnership {
  param(
    [Parameter(Mandatory = $true)]$Process,
    [Parameter(Mandatory = $true)][hashtable]$ExpectedPaths
  )

  $name = [string]$Process.Name
  if (-not $ExpectedPaths.ContainsKey($name)) {
    return "Foreign"
  }

  $executablePath = [string]$Process.ExecutablePath
  if ([string]::IsNullOrWhiteSpace($executablePath)) {
    return "Unknown"
  }

  try {
    $normalizedExecutablePath = [System.IO.Path]::GetFullPath($executablePath)
  } catch {
    return "Unknown"
  }

  if ([string]::Equals($normalizedExecutablePath, $ExpectedPaths[$name], $comparison)) {
    return "Owned"
  }
  return "Foreign"
}

function Get-OwnedProcesses {
  $owned = @()
  foreach ($processName in $processNames) {
    $processes = @(
      Get-CimInstance -ClassName Win32_Process -Filter "Name = '$processName'" -ErrorAction Stop
    )
    foreach ($process in $processes) {
      $ownership = Get-ProcessOwnership -Process $process -ExpectedPaths $expectedPaths
      switch ($ownership) {
        "Owned" {
          $owned += $process
        }
        "Foreign" {
          continue
        }
        default {
          throw "Cannot determine the executable path for $($process.Name) (PID $($process.ProcessId)); refusing to modify the install directory"
        }
      }
    }
  }
  return $owned
}

foreach ($process in @(Get-OwnedProcesses)) {
  $processId = [uint32]$process.ProcessId

  # Re-read the PID immediately before stopping it so a stale enumeration cannot
  # target a process that does not have the expected name and executable path.
  $current = @(
    Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $processId" -ErrorAction Stop
  ) | Select-Object -First 1
  if ($null -eq $current) {
    continue
  }
  $currentOwnership = Get-ProcessOwnership -Process $current -ExpectedPaths $expectedPaths
  if ($currentOwnership -eq "Unknown") {
    throw "Cannot revalidate the executable path for $($current.Name) (PID $processId)"
  }
  if ($currentOwnership -ne "Owned") {
    throw "Process $processId changed identity before it could be stopped"
  }

  Stop-Process -Id $processId -Force -ErrorAction Stop
}

$remaining = @()
for ($attempt = 0; $attempt -lt 50; $attempt++) {
  $remaining = @(Get-OwnedProcesses)
  if ($remaining.Count -eq 0) {
    return
  }
  Start-Sleep -Milliseconds 100
}

$details = $remaining | ForEach-Object { "$($_.Name) (PID $($_.ProcessId))" }
throw "U-API Connect processes are still running: $($details -join ', ')"
