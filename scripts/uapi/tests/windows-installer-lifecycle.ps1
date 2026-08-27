[CmdletBinding()]
param(
  [string]$InstallerPattern = "dist/uapi/windows/UAPIConnect-*-windows-x64-setup.exe"
)

$ErrorActionPreference = "Stop"

$installers = @(Get-ChildItem -Path $InstallerPattern)
if ($installers.Count -ne 1) {
  throw "Expected exactly one Windows installer for ${InstallerPattern}; found $($installers.Count)"
}
$installer = $installers[0]

$installDir = Join-Path $env:LOCALAPPDATA "Programs\U-API Connect"
$launcher = Join-Path $installDir "codex-plus-plus.exe"
$manager = Join-Path $installDir "codex-plus-plus-manager.exe"
$uninstaller = Join-Path $installDir "uninstall.exe"
$quietUninstallBootstrap = Join-Path $installDir "quiet-uninstall-bootstrap.ps1"
$desktop = [Environment]::GetFolderPath("Desktop")
$programs = [Environment]::GetFolderPath("Programs")
$desktopLauncher = Join-Path $desktop "U-API Connect.lnk"
$desktopManager = Join-Path $desktop "U-API Connect 设置.lnk"
$startMenuDir = Join-Path $programs "U-API Connect"
$startMenuLauncher = Join-Path $startMenuDir "U-API Connect.lnk"
$startMenuManager = Join-Path $startMenuDir "U-API Connect 设置.lnk"
$startMenuUninstaller = Join-Path $startMenuDir "卸载 U-API Connect.lnk"
$productKey = "Registry::HKEY_CURRENT_USER\Software\UAPIConnect"
$uninstallKey = "Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect"
$protocolRoot = "Registry::HKEY_CURRENT_USER\Software\Classes\uapiconnect"
$protocolKey = "Registry::HKEY_CURRENT_USER\Software\Classes\uapiconnect\shell\open\command"

$requiredPaths = @(
  $launcher,
  $manager,
  $uninstaller,
  $quietUninstallBootstrap,
  $desktopLauncher,
  $desktopManager,
  $startMenuLauncher,
  $startMenuManager,
  $startMenuUninstaller,
  $productKey,
  $uninstallKey,
  $protocolRoot,
  $protocolKey
)

function Assert-RequiredPaths {
  $missing = $requiredPaths | Where-Object { -not (Test-Path -LiteralPath $_) }
  if ($missing) {
    throw "Installer omitted: $($missing -join ', ')"
  }
}

function Assert-AuthenticodeSignatures {
  if ($env:UAPI_WINDOWS_SIGNING -ne "1") {
    return
  }

  $expectedThumbprint = ($env:UAPI_SIGNING_THUMBPRINT -replace "\s", "").ToUpperInvariant()
  if ([string]::IsNullOrWhiteSpace($expectedThumbprint)) {
    throw "Signed lifecycle validation requires UAPI_SIGNING_THUMBPRINT"
  }

  $signedPaths = @(
    $installer.FullName,
    $launcher,
    $manager,
    $uninstaller
  )
  foreach ($path in $signedPaths) {
    $signature = Get-AuthenticodeSignature -FilePath $path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
      throw "Invalid Authenticode signature for ${path}: $($signature.Status)"
    }
    if ($null -eq $signature.SignerCertificate) {
      throw "Authenticode signer certificate is missing for $path"
    }
    $actualThumbprint = ($signature.SignerCertificate.Thumbprint -replace "\s", "").ToUpperInvariant()
    if ($actualThumbprint -ne $expectedThumbprint) {
      throw "Unexpected Authenticode signer for ${path}: $actualThumbprint"
    }
  }
}

# WScript.Shell cannot reliably reopen shortcuts whose names contain CJK text.
$shell = New-Object -ComObject Shell.Application
function Get-ShortcutTarget {
  param([string]$ShortcutPath)

  $folder = $shell.Namespace((Split-Path -Parent $ShortcutPath))
  if ($null -eq $folder) {
    throw "Cannot open shortcut folder: $ShortcutPath"
  }
  $item = $folder.ParseName((Split-Path -Leaf $ShortcutPath))
  if ($null -eq $item) {
    throw "Cannot find shortcut through Windows Shell: $ShortcutPath"
  }
  $link = $item.GetLink
  if ($null -eq $link -or [string]::IsNullOrWhiteSpace($link.Path)) {
    throw "Cannot resolve shortcut through Windows Shell: $ShortcutPath"
  }
  return [System.IO.Path]::GetFullPath([string]$link.Path)
}

$shortcutTargets = @(
  [pscustomobject]@{ Path = $desktopLauncher; Target = $launcher }
  [pscustomobject]@{ Path = $desktopManager; Target = $manager }
  [pscustomobject]@{ Path = $startMenuLauncher; Target = $launcher }
  [pscustomobject]@{ Path = $startMenuManager; Target = $manager }
  [pscustomobject]@{ Path = $startMenuUninstaller; Target = $uninstaller }
)

function Assert-ShortcutTargets {
  foreach ($entry in $shortcutTargets) {
    $target = Get-ShortcutTarget -ShortcutPath $entry.Path
    $expectedTarget = [System.IO.Path]::GetFullPath($entry.Target)
    if ($target -ne $expectedTarget) {
      throw "Unexpected shortcut target for $($entry.Path): $target; expected $expectedTarget"
    }
  }
}

function Get-RegisteredUninstallCommands {
  $key = Get-Item -LiteralPath $uninstallKey
  $uninstallCommand = [string]$key.GetValue("UninstallString")
  $quietUninstallCommand = [string]$key.GetValue("QuietUninstallString")
  $expectedUninstallCommand = '"' + $uninstaller + '"'
  $windowsPowerShell = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
  $expectedQuietUninstallCommand = '"' + $windowsPowerShell +
    '" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "' +
    $quietUninstallBootstrap + '" -InstallDir "' + $installDir + '"'

  if ($uninstallCommand -ne $expectedUninstallCommand) {
    throw "Unexpected UninstallString: $uninstallCommand"
  }
  if ($quietUninstallCommand -ne $expectedQuietUninstallCommand) {
    throw "Unexpected QuietUninstallString: $quietUninstallCommand"
  }
  return [pscustomobject]@{
    Interactive = $uninstallCommand
    Quiet = $quietUninstallCommand
  }
}

function Start-RegisteredCommand {
  param([Parameter(Mandatory = $true)][string]$CommandLine)

  $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $env:ComSpec
  $startInfo.UseShellExecute = $false
  # cmd.exe 的 /S /C 对首尾引号有自己的解析规则。ProcessStartInfo.ArgumentList
  # 会再次转义注册表命令中的引号，最终把 \"C:\...\" 当成字面文件名。
  # 用完整 Arguments 并额外包一层引号，等价于 Windows 卸载入口执行原始命令。
  $startInfo.Arguments = '/d /s /c "' + $CommandLine + '"'
  $process = [System.Diagnostics.Process]::Start($startInfo)
  if ($null -eq $process) {
    throw "Failed to start registered command: $CommandLine"
  }
  $process.WaitForExit()
  return $process.ExitCode
}

function Assert-ForeignSameNameProcess {
  param(
    [Parameter(Mandatory = $true)]$Entry,
    [Parameter(Mandatory = $true)][string]$Phase
  )

  if ($Entry.Process.HasExited) {
    throw "$Phase stopped the foreign $($Entry.Name) process"
  }
  $cimProcess = @(
    Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $($Entry.Process.Id)"
  ) | Select-Object -First 1
  if ($null -eq $cimProcess) {
    throw "Cannot query foreign $($Entry.Name) during $Phase"
  }
  if (([string]$cimProcess.Name) -ine $Entry.Name) {
    throw "Foreign process fixture has WMI name '$($cimProcess.Name)', expected '$($Entry.Name)'"
  }
  $actualPath = [System.IO.Path]::GetFullPath([string]$cimProcess.ExecutablePath)
  $expectedPath = [System.IO.Path]::GetFullPath([string]$Entry.Path)
  if ($actualPath -ine $expectedPath) {
    throw "Foreign $($Entry.Name) path changed during ${Phase}: $actualPath"
  }
}

function Assert-ForeignSameNameProcesses {
  param(
    [Parameter(Mandatory = $true)][array]$Entries,
    [Parameter(Mandatory = $true)][string]$Phase
  )

  foreach ($entry in $Entries) {
    Assert-ForeignSameNameProcess -Entry $entry -Phase $Phase
  }
}

$install = Start-Process -FilePath $installer.FullName -ArgumentList "/S" -Wait -PassThru
if ($install.ExitCode -ne 0) {
  throw "Installer exited with $($install.ExitCode)"
}

Assert-RequiredPaths
Assert-ShortcutTargets
Assert-AuthenticodeSignatures
$registeredCommands = Get-RegisteredUninstallCommands

$protocolCommand = (Get-Item -LiteralPath $protocolKey).GetValue("")
$expectedProtocolCommand = '"' + $manager + '" "%1"'
if ($protocolCommand -ne $expectedProtocolCommand) {
  throw "Unexpected protocol command: $protocolCommand"
}

$foreignDir = Join-Path $env:RUNNER_TEMP "uapi-foreign-same-name-$PID"
$pingExecutable = Join-Path $env:SystemRoot "System32\PING.EXE"
$foreignEntries = @()
$helper = $null
$uninstallHelper = $null

try {
  New-Item -ItemType Directory -Path $foreignDir -Force | Out-Null
  foreach ($foreignName in @("codex-plus-plus.exe", "codex-plus-plus-manager.exe")) {
    $foreignPath = Join-Path $foreignDir $foreignName
    Copy-Item -LiteralPath $pingExecutable -Destination $foreignPath
    $foreignEntries += [pscustomobject]@{
      Name = $foreignName
      Path = $foreignPath
      Process = (Start-Process -FilePath $foreignPath -ArgumentList "-t 127.0.0.1" -PassThru)
    }
  }

  Start-Sleep -Seconds 2
  Assert-ForeignSameNameProcesses -Entries $foreignEntries -Phase "before upgrade"

  $helper = Start-Process -FilePath $launcher -ArgumentList "--helper-only" -PassThru
  Start-Sleep -Seconds 2
  if ($helper.HasExited) {
    throw "Installed launcher helper exited before upgrade"
  }
  $upgrade = Start-Process -FilePath $installer.FullName -ArgumentList "/S" -Wait -PassThru
  if ($upgrade.ExitCode -ne 0) {
    throw "Upgrade exited with $($upgrade.ExitCode)"
  }
  if (-not $helper.WaitForExit(5000)) {
    Stop-Process -Id $helper.Id -Force
    throw "Upgrade did not stop the running launcher"
  }
  Assert-ForeignSameNameProcesses -Entries $foreignEntries -Phase "after upgrade"

  Assert-RequiredPaths
  Assert-ShortcutTargets
  Assert-AuthenticodeSignatures
  $registeredCommands = Get-RegisteredUninstallCommands

  # An exclusive handle makes CreateProcess fail for --uninstall-cleanup. This
  # exercises the actual NSIS cleanup_failed -> SetErrorLevel 2 path: the copied
  # uninstaller's status must reach the registered command, and no owned file or
  # registration may be removed.
  $managerLock = [System.IO.File]::Open(
    $manager,
    [System.IO.FileMode]::Open,
    [System.IO.FileAccess]::Read,
    [System.IO.FileShare]::None
  )
  try {
    $failedUninstallExitCode = Start-RegisteredCommand -CommandLine $registeredCommands.Quiet
    if ($failedUninstallExitCode -ne 2) {
      throw "Quiet uninstall bootstrap returned $failedUninstallExitCode for cleanup failure; expected 2"
    }
    $missingAfterFailedUninstall = @(
      $launcher,
      $manager,
      $uninstaller,
      $quietUninstallBootstrap,
      $desktopLauncher,
      $desktopManager,
      $startMenuDir,
      $startMenuLauncher,
      $startMenuManager,
      $startMenuUninstaller,
      $productKey,
      $uninstallKey,
      $protocolRoot,
      $protocolKey
    ) | Where-Object { -not (Test-Path -LiteralPath $_) }
    if ($missingAfterFailedUninstall) {
      throw "Failed quiet uninstall removed owned state: $($missingAfterFailedUninstall -join ', ')"
    }
    Assert-ForeignSameNameProcesses -Entries $foreignEntries -Phase "after failed uninstall"
  } finally {
    $managerLock.Dispose()
  }

  Assert-RequiredPaths
  Assert-AuthenticodeSignatures

  $uninstallHelper = Start-Process -FilePath $launcher -ArgumentList "--helper-only" -PassThru
  Start-Sleep -Seconds 2
  if ($uninstallHelper.HasExited) {
    throw "Installed launcher helper exited before uninstall"
  }

  $uninstallExitCode = Start-RegisteredCommand -CommandLine $registeredCommands.Quiet
  if ($uninstallExitCode -ne 0) {
    throw "Registered uninstaller exited with $uninstallExitCode"
  }
  if (-not $uninstallHelper.WaitForExit(5000)) {
    Stop-Process -Id $uninstallHelper.Id -Force
    throw "Uninstall did not stop the running launcher"
  }
  Assert-ForeignSameNameProcesses -Entries $foreignEntries -Phase "after uninstall"

  for ($attempt = 0; $attempt -lt 40; $attempt++) {
    $remaining = @(
      $installDir,
      $launcher,
      $manager,
      $uninstaller,
      $quietUninstallBootstrap,
      $desktopLauncher,
      $desktopManager,
      $startMenuDir,
      $startMenuLauncher,
      $startMenuManager,
      $startMenuUninstaller,
      $productKey,
      $uninstallKey,
      $protocolRoot,
      $protocolKey
    ) | Where-Object { Test-Path -LiteralPath $_ }
    if (-not $remaining) {
      break
    }
    Start-Sleep -Milliseconds 500
  }
  if ($remaining) {
    throw "Uninstaller left behind: $($remaining -join ', ')"
  }
} finally {
  foreach ($ownedTestProcess in @($helper, $uninstallHelper)) {
    if ($null -ne $ownedTestProcess -and -not $ownedTestProcess.HasExited) {
      Stop-Process -Id $ownedTestProcess.Id -Force -ErrorAction SilentlyContinue
    }
  }
  foreach ($entry in $foreignEntries) {
    if (-not $entry.Process.HasExited) {
      Stop-Process -Id $entry.Process.Id -Force -ErrorAction SilentlyContinue
      $entry.Process.WaitForExit(5000)
    }
  }
  foreach ($foreignName in @("codex-plus-plus.exe", "codex-plus-plus-manager.exe")) {
    $foreignPath = Join-Path $foreignDir $foreignName
    if (Test-Path -LiteralPath $foreignPath) {
      Remove-Item -LiteralPath $foreignPath -Force
    }
  }
  if (Test-Path -LiteralPath $foreignDir) {
    Remove-Item -LiteralPath $foreignDir -Force
  }
}
