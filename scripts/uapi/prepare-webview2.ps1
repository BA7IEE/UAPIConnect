[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$OutputPath
)

$ErrorActionPreference = "Stop"
$bootstrapperUrl = "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
$outputDirectory = Split-Path -Parent $OutputPath

if ([string]::IsNullOrWhiteSpace($outputDirectory)) {
  throw "WebView2 bootstrapper output path must include a directory"
}

New-Item -ItemType Directory -Force $outputDirectory | Out-Null

try {
  Invoke-WebRequest -Uri $bootstrapperUrl -OutFile $OutputPath

  $signature = Get-AuthenticodeSignature -FilePath $OutputPath
  $signerSubject = if ($null -eq $signature.SignerCertificate) {
    ""
  } else {
    [string]$signature.SignerCertificate.Subject
  }

  if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    throw "Downloaded WebView2 bootstrapper has an invalid Authenticode signature: $($signature.Status)"
  }
  if ($signerSubject -notmatch "(?:^|,\s*)O=Microsoft Corporation(?:,|$)") {
    throw "Downloaded WebView2 bootstrapper is not signed by Microsoft Corporation"
  }

  (Resolve-Path -LiteralPath $OutputPath).Path
} catch {
  if (Test-Path -LiteralPath $OutputPath) {
    Remove-Item -LiteralPath $OutputPath -Force
  }
  throw
}
