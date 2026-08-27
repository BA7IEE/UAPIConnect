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
  $expectedQuietUninstallCommand = $expectedUninstallCommand + " /S"

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
  $startInfo.ArgumentList.Add("/d")
  $startInfo.ArgumentList.Add("/s")
  $startInfo.ArgumentList.Add("/c")
  $startInfo.ArgumentList.Add($CommandLine)
  $process = [System.Diagnostics.Process]::Start($startInfo)
  if ($null -eq $process) {
    throw "Failed to start registered command: $CommandLine"
  }
  $process.WaitForExit()
  return $process.ExitCode
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

Assert-RequiredPaths
Assert-ShortcutTargets
Assert-AuthenticodeSignatures
$registeredCommands = Get-RegisteredUninstallCommands

$uninstallExitCode = Start-RegisteredCommand -CommandLine $registeredCommands.Quiet
if ($uninstallExitCode -ne 0) {
  throw "Registered uninstaller exited with $uninstallExitCode"
}

for ($attempt = 0; $attempt -lt 40; $attempt++) {
  $remaining = @(
    $installDir,
    $launcher,
    $manager,
    $uninstaller,
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
