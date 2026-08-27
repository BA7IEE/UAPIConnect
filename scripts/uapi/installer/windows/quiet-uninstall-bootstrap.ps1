[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$InstallDir
)

$ErrorActionPreference = "Stop"
$comparison = [System.StringComparison]::OrdinalIgnoreCase
$temporaryDirectory = $null
$childExitCode = 1

try {
  $normalizedInstallDir = [System.IO.Path]::GetFullPath($InstallDir).TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
  )
  $installRoot = [System.IO.Path]::GetPathRoot($normalizedInstallDir).TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
  )
  if ([string]::IsNullOrWhiteSpace($normalizedInstallDir) -or
      [string]::Equals($normalizedInstallDir, $installRoot, $comparison)) {
    throw "InstallDir does not resolve to a safe application directory"
  }

  $expectedBootstrap = [System.IO.Path]::GetFullPath(
    (Join-Path $normalizedInstallDir "quiet-uninstall-bootstrap.ps1")
  )
  $actualBootstrap = [System.IO.Path]::GetFullPath($PSCommandPath)
  if (-not [string]::Equals($actualBootstrap, $expectedBootstrap, $comparison)) {
    throw "The uninstall bootstrap must run from the managed install directory"
  }

  $installedUninstaller = [System.IO.Path]::GetFullPath(
    (Join-Path $normalizedInstallDir "uninstall.exe")
  )
  if (-not (Test-Path -LiteralPath $installedUninstaller -PathType Leaf)) {
    throw "The installed U-API Connect uninstaller is missing"
  }

  $temporaryDirectory = Join-Path (
    [System.IO.Path]::GetTempPath()
  ) ("uapi-uninstall-{0}-{1}" -f $PID, [guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
  $temporaryUninstaller = Join-Path $temporaryDirectory "uninstall.exe"
  Copy-Item -LiteralPath $installedUninstaller -Destination $temporaryUninstaller

  # NSIS treats the final _?= argument as the original install directory and,
  # crucially, does not self-copy again. The process we wait for is therefore
  # the process that runs the uninstall section and owns its real exit code.
  $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $temporaryUninstaller
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.Arguments = "/S _?=$normalizedInstallDir"
  $process = [System.Diagnostics.Process]::Start($startInfo)
  if ($null -eq $process) {
    throw "Failed to start the copied U-API Connect uninstaller"
  }
  $process.WaitForExit()
  $childExitCode = $process.ExitCode

  if ($childExitCode -eq 0) {
    # The uninstall section normally removes this script. If Windows kept the
    # parsed script open, remove it now that the child uninstaller has exited.
    if (Test-Path -LiteralPath $actualBootstrap) {
      Remove-Item -LiteralPath $actualBootstrap -Force
    }
    if (Test-Path -LiteralPath $normalizedInstallDir) {
      Remove-Item -LiteralPath $normalizedInstallDir -Force
    }
    if (Test-Path -LiteralPath $normalizedInstallDir) {
      throw "The U-API Connect install directory is not empty after uninstall"
    }
  }
} catch {
  [Console]::Error.WriteLine("U-API Connect uninstall bootstrap failed: $($_.Exception.Message)")
  $childExitCode = 1
} finally {
  if ($null -ne $temporaryDirectory -and (Test-Path -LiteralPath $temporaryDirectory)) {
    try {
      $temporaryUninstaller = Join-Path $temporaryDirectory "uninstall.exe"
      if (Test-Path -LiteralPath $temporaryUninstaller) {
        Remove-Item -LiteralPath $temporaryUninstaller -Force
      }
      Remove-Item -LiteralPath $temporaryDirectory -Force
    } catch {
      [Console]::Error.WriteLine("Temporary uninstaller cleanup failed: $($_.Exception.Message)")
    }
  }
}

exit $childExitCode
