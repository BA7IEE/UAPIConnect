import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import test from "node:test";

const manifest = JSON.parse(
  readFileSync(new URL("../../../distribution/uapi-connect.json", import.meta.url), "utf8"),
);

const rustPolicy = readFileSync(
  new URL("../../../crates/codex-plus-core/src/distribution.rs", import.meta.url),
  "utf8",
);
const managedIntegration = readFileSync(
  new URL("../../../crates/codex-plus-core/src/uapi.rs", import.meta.url),
  "utf8",
);
const managerEntry = readFileSync(
  new URL("../../../apps/codex-plus-manager/src/main.tsx", import.meta.url),
  "utf8",
);
const windowsInstaller = readFileSync(
  new URL("../installer/windows/UAPIConnect.nsi", import.meta.url),
  "utf8",
);
const windowsProcessStopper = readFileSync(
  new URL("../installer/windows/stop-owned-processes.ps1", import.meta.url),
  "utf8",
);
const windowsQuietUninstallBootstrap = readFileSync(
  new URL("../installer/windows/quiet-uninstall-bootstrap.ps1", import.meta.url),
  "utf8",
);
const windowsInstallRepair = readFileSync(
  new URL("../../../crates/codex-plus-core/src/install/windows.rs", import.meta.url),
  "utf8",
);
const windowsLifecycle = readFileSync(
  new URL("./windows-installer-lifecycle.ps1", import.meta.url),
  "utf8",
);
const prepareWebView2 = readFileSync(new URL("../prepare-webview2.ps1", import.meta.url), "utf8");
const buildWorkflow = readFileSync(
  new URL("../../../.github/workflows/uapi-build.yml", import.meta.url),
  "utf8",
);
const releaseWorkflow = readFileSync(
  new URL("../../../.github/workflows/release-assets.yml", import.meta.url),
  "utf8",
);
const macosPackager = readFileSync(new URL("../package-macos-dmg.sh", import.meta.url), "utf8");
const localMacosBuild = readFileSync(new URL("../build-local-macos.sh", import.meta.url), "utf8");
const upstreamSurfaceAudit = readFileSync(
  new URL("../audit-upstream-surface.sh", import.meta.url),
  "utf8",
);
const readme = readFileSync(new URL("../../../README.md", import.meta.url), "utf8");
const windowsAcceptance = readFileSync(
  new URL("../../../docs/uapi/WINDOWS_ACCEPTANCE.md", import.meta.url),
  "utf8",
);

function rustString(name) {
  const match = rustPolicy.match(new RegExp(`pub const ${name}: &str = "([^"]*)";`));
  assert.ok(match, `missing Rust distribution constant ${name}`);
  return match[1];
}

function rustBool(name) {
  const match = rustPolicy.match(new RegExp(`pub const ${name}: bool = (true|false);`));
  assert.ok(match, `missing Rust distribution constant ${name}`);
  return match[1] === "true";
}

test("distribution fixes one NewAPI endpoint without credentials", () => {
  assert.equal(manifest.fixedBaseUrl, "https://token.u-studio.cn/v1");
  assert.equal(manifest.fixedProviderId, "uapi_connect");
  assert.equal(manifest.features.fixedProviderEdition, true);
  assert.equal(manifest.features.adsEnabled, false);
  assert.equal(manifest.features.updatesEnabled, false);
  assert.doesNotMatch(JSON.stringify(manifest), /sk-[A-Za-z0-9_-]{16,}/);
});

test("Rust policy mirrors the public manifest", () => {
  assert.equal(rustString("PRODUCT_NAME"), manifest.productName);
  assert.equal(rustString("FIXED_PROVIDER_ID"), manifest.fixedProviderId);
  assert.equal(rustString("FIXED_BASE_URL"), manifest.fixedBaseUrl);
  assert.equal(rustString("MANAGER_BUNDLE_ID"), manifest.managerBundleId);
  assert.equal(rustBool("FIXED_PROVIDER_EDITION"), manifest.features.fixedProviderEdition);
  assert.equal(rustBool("ADS_ENABLED"), manifest.features.adsEnabled);
  assert.equal(rustBool("UPDATES_ENABLED"), manifest.features.updatesEnabled);
});

test("managed integration is dynamic and provider identifiers stay paired", () => {
  assert.match(managedIntegration, /format!\(\s*"\{\}\/models"/);
  assert.match(managedIntegration, /supported_endpoint_types/);
  assert.match(managedIntegration, /openai-response/);
  assert.match(managedIntegration, /\[model_providers\.\{\}\]/);
  assert.match(managedIntegration, /distribution::FIXED_PROVIDER_ID/);
  assert.doesNotMatch(managedIntegration, /const\s+DEFAULT_MODEL/);
  assert.doesNotMatch(managedIntegration, /DEFAULT[^\n]*gpt-5\.5|gpt-5\.5[^\n]*DEFAULT/i);
});

test("production manager entry uses the isolated U-API shell", () => {
  assert.match(managerEntry, /\.\/uapi\/UapiApp/);
  assert.doesNotMatch(managerEntry, /from\s+["']\.\/App["']/);
  assert.deepEqual(manifest.visibleRoutes, ["overview", "connection", "maintenance", "about"]);
});

test("pinned upstream preview is reproducible and cannot pass the release gate", () => {
  const base = readFileSync(new URL("../../../distribution/upstream-base.txt", import.meta.url), "utf8").trim();
  assert.match(base, /^(?:[0-9a-f]{40}|v\d+\.\d+\.\d+)$/);
  assert.match(upstreamSurfaceAudit, /distribution\/upstream-base\.txt/);
  assert.match(upstreamSurfaceAudit, /git merge-base --is-ancestor "\$base_commit" HEAD/);
  assert.match(releaseWorkflow, /UAPI_REQUIRE_STABLE_UPSTREAM: "1"/);
  assert.match(upstreamSurfaceAudit, /unreleased upstream preview cannot be published/);
  assert.match(managedIntegration, /settings\.codex_app_answer_outline_enabled = false/);

  // 只在本轮预览基线上检查拒绝路径，下一稳定 tag 的正常发布不应被测试误拦。
  if (base === "11179a0100afb3b04c0342ccc9a3159fa25f8b4d") {
    const result = spawnSync("bash", ["scripts/uapi/audit-upstream-surface.sh"], {
      cwd: new URL("../../../", import.meta.url),
      env: { ...process.env, UAPI_REQUIRE_STABLE_UPSTREAM: "1" },
      encoding: "utf8",
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /unreleased upstream preview cannot be published/);
  }
});

test("Windows uninstall registration and cleanup are fail closed", () => {
  assert.ok(
    windowsInstaller.includes(String.raw`"UninstallString" '"$INSTDIR\uninstall.exe"'`),
    "UninstallString must quote the path because the install directory contains a space",
  );
  assert.match(
    windowsInstaller,
    /"QuietUninstallString"[^\r\n]+WindowsPowerShell\\v1\.0\\powershell\.exe[^\r\n]+quiet-uninstall-bootstrap\.ps1[^\r\n]+-InstallDir "\$INSTDIR"/,
  );
  assert.doesNotMatch(
    windowsInstaller,
    /"QuietUninstallString"\s+'"\$INSTDIR\\uninstall\.exe" \/S'/,
  );
  assert.match(
    windowsInstaller,
    /File \/oname=quiet-uninstall-bootstrap\.ps1[^\r\n]+quiet-uninstall-bootstrap\.ps1/,
  );
  assert.match(windowsInstaller, /--uninstall-cleanup/);
  assert.match(windowsInstaller, /IfErrors cleanup_failed/);
  assert.match(windowsInstaller, /SetErrorLevel 2/);
  assert.match(windowsInstaller, /cleanup_failed:[\s\S]*?Abort/);

  const cleanup = windowsInstaller.indexOf("--uninstall-cleanup");
  const deleteManager = windowsInstaller.indexOf(String.raw`Delete "$INSTDIR\codex-plus-plus-manager.exe"`);
  assert.ok(cleanup >= 0 && deleteManager > cleanup, "program files must survive until cleanup succeeds");
});

test("Windows installer stops only processes owned by its install directory", () => {
  assert.doesNotMatch(windowsInstaller, /taskkill|\/IM\s+codex-plus-plus/i);
  assert.equal(
    (windowsInstaller.match(/File \/oname=stop-owned-processes\.ps1/g) ?? []).length,
    2,
    "installer and uninstaller must embed the same process ownership helper",
  );
  assert.equal(
    (
      windowsInstaller.match(
        /-File "\$PLUGINSDIR\\stop-owned-processes\.ps1" -InstallDir "\$INSTDIR"/g,
      ) ?? []
    ).length,
    2,
    "installer and uninstaller must scope process stopping to their own install directory",
  );
  assert.match(windowsInstaller, /install_stop_failed:[\s\S]*?SetErrorLevel 2[\s\S]*?Abort/);
  assert.match(windowsInstaller, /uninstall_stop_failed:[\s\S]*?SetErrorLevel 2[\s\S]*?Abort/);

  const installStop = windowsInstaller.indexOf("-InstallDir");
  const writeLauncher = windowsInstaller.indexOf(
    'File "${ROOT}\\dist\\uapi\\windows\\app\\codex-plus-plus.exe"',
  );
  assert.ok(
    installStop >= 0 && writeLauncher > installStop,
    "owned processes must stop before install writes",
  );

  const uninstallSection = windowsInstaller.indexOf('Section "Uninstall"');
  const uninstallStop = windowsInstaller.indexOf("-InstallDir", uninstallSection);
  const cleanup = windowsInstaller.indexOf("--uninstall-cleanup", uninstallSection);
  assert.ok(
    uninstallStop > uninstallSection && cleanup > uninstallStop,
    "owned processes must stop before uninstall cleanup",
  );

  assert.match(windowsProcessStopper, /"codex-plus-plus\.exe"/);
  assert.match(windowsProcessStopper, /"codex-plus-plus-manager\.exe"/);
  assert.match(windowsProcessStopper, /Win32_Process/);
  assert.match(windowsProcessStopper, /ExecutablePath/);
  assert.match(windowsProcessStopper, /return "Owned"/);
  assert.match(windowsProcessStopper, /return "Foreign"/);
  assert.match(windowsProcessStopper, /return "Unknown"/);
  assert.match(
    windowsProcessStopper,
    /IsNullOrWhiteSpace\(\$executablePath\)[\s\S]*?return "Unknown"/,
  );
  assert.match(
    windowsProcessStopper,
    /default \{\s*throw "Cannot determine the executable path/,
  );
  assert.match(windowsProcessStopper, /Join-Path \$normalizedInstallDir \$processName/);
  assert.match(windowsProcessStopper, /StringComparison\]::OrdinalIgnoreCase/);
  assert.match(
    windowsProcessStopper,
    /\[string\]::Equals\(\$normalizedExecutablePath, \$ExpectedPaths\[\$name\], \$comparison\)/,
  );
  assert.match(windowsProcessStopper, /Stop-Process -Id \$processId -Force -ErrorAction Stop/);
  assert.match(windowsProcessStopper, /ProcessId = \$processId/);
  assert.match(windowsProcessStopper, /Get-OwnedProcesses[\s\S]*?remaining[\s\S]*?throw/);
});

test("Windows CI validates scoped processes and propagated quiet uninstall status", () => {
  assert.match(windowsLifecycle, /\$installers\.Count -ne 1/);
  assert.match(windowsLifecycle, /\.GetValue\("UninstallString"\)/);
  assert.match(windowsLifecycle, /\.GetValue\("QuietUninstallString"\)/);
  assert.match(
    windowsLifecycle,
    /Start-RegisteredCommand -CommandLine \$registeredCommands\.Quiet/,
  );
  assert.match(windowsLifecycle, /uapi-foreign-same-name-\$PID/);
  assert.match(windowsLifecycle, /System32\\PING\.EXE/);
  assert.match(windowsLifecycle, /@\("codex-plus-plus\.exe", "codex-plus-plus-manager\.exe"\)/);
  assert.match(windowsLifecycle, /\(\[string\]\$cimProcess\.Name\) -ine \$Entry\.Name/);
  assert.match(windowsLifecycle, /\$actualPath -ine \$expectedPath/);
  assert.match(windowsLifecycle, /-Phase "before upgrade"/);
  assert.match(windowsLifecycle, /-Phase "after upgrade"/);
  assert.match(windowsLifecycle, /-Phase "after uninstall"/);
  assert.match(windowsLifecycle, /\[System\.IO\.FileShare\]::None/);
  assert.match(windowsLifecycle, /cleanup_failed -> SetErrorLevel 2/);
  assert.match(windowsLifecycle, /\$failedUninstallExitCode -ne 2/);
  assert.match(windowsLifecycle, /Failed quiet uninstall removed owned state/);

  assert.match(windowsQuietUninstallBootstrap, /Copy-Item[\s\S]*?\$temporaryUninstaller/);
  assert.match(windowsQuietUninstallBootstrap, /Arguments = "\/S _\?=\$normalizedInstallDir"/);
  assert.match(windowsQuietUninstallBootstrap, /\$childExitCode = \$process\.ExitCode/);
  assert.match(
    windowsQuietUninstallBootstrap,
    /if \(\$childExitCode -eq 0\)[\s\S]*?Remove-Item -LiteralPath \$actualBootstrap/,
  );
  assert.match(windowsQuietUninstallBootstrap, /exit \$childExitCode/);

  assert.match(windowsInstallRepair, /quiet-uninstall-bootstrap\.ps1/);
  assert.match(windowsInstallRepair, /WindowsPowerShell[\\/]+v1\.0[\\/]+powershell\.exe/);
  assert.doesNotMatch(windowsInstallRepair, /format!\("\{uninstall_command\} \/S"\)/);
  assert.match(buildWorkflow, /scripts\/uapi\/tests\/windows-installer-lifecycle\.ps1/);
});

test("Windows installer provisions a verified WebView2 runtime before the application", () => {
  assert.match(windowsInstaller, /WEBVIEW2_APP_GUID "\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5\}"/);
  assert.match(
    windowsInstaller,
    /SetRegView 32[\s\S]*?HKLM "SOFTWARE\\Microsoft\\EdgeUpdate\\Clients\\\$\{WEBVIEW2_APP_GUID\}" "pv"[\s\S]*?SetRegView Default/,
  );
  assert.doesNotMatch(windowsInstaller, /SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate/);
  assert.match(
    windowsInstaller,
    /HKCU "Software\\Microsoft\\EdgeUpdate\\Clients\\\$\{WEBVIEW2_APP_GUID\}" "pv"/,
  );
  assert.match(windowsInstaller, /"0\.0\.0\.0"/);
  assert.match(
    windowsInstaller,
    /InitPluginsDir\s+SetOutPath "\$PLUGINSDIR"\s+File \/oname=MicrosoftEdgeWebView2Setup\.exe/,
  );
  assert.match(windowsInstaller, /MicrosoftEdgeWebView2Setup\.exe" \/silent \/install/);
  assert.match(
    windowsInstaller,
    /ClearErrors\s+ExecWait[^\n]+MicrosoftEdgeWebView2Setup\.exe[^\n]+\s+IfErrors webview2_exec_failed/,
  );
  assert.match(windowsInstaller, /webview2_failed:[\s\S]*?SetErrorLevel 2[\s\S]*?Abort/);
  assert.ok(
    windowsInstaller.indexOf('Section "-WebView2 Runtime"') <
      windowsInstaller.indexOf('Section "Install"'),
    "WebView2 must be ready before application files are written",
  );

  assert.match(prepareWebView2, /https:\/\/go\.microsoft\.com\/fwlink\/p\/\?LinkId=2124703/);
  assert.match(prepareWebView2, /Get-AuthenticodeSignature/);
  assert.match(prepareWebView2, /SignatureStatus\]::Valid/);
  assert.match(prepareWebView2, /O=Microsoft Corporation/);
  for (const workflow of [buildWorkflow, releaseWorkflow]) {
    assert.match(workflow, /scripts\/uapi\/prepare-webview2\.ps1/);
    assert.match(workflow, /DWEBVIEW2_BOOTSTRAPPER=\$env:UAPI_WEBVIEW2_BOOTSTRAPPER/);
  }
});

test("Windows Authenticode signing is optional, release-only, and fail closed", () => {
  assert.doesNotMatch(buildWorkflow, /WINDOWS_CERTIFICATE|UAPI_SIGNING_THUMBPRINT|SIGNTOOL_PATH/);
  assert.match(releaseWorkflow, /WINDOWS_CERTIFICATE: \$\{\{ secrets\.WINDOWS_CERTIFICATE \}\}/);
  assert.match(
    releaseWorkflow,
    /WINDOWS_CERTIFICATE_PASSWORD: \$\{\{ secrets\.WINDOWS_CERTIFICATE_PASSWORD \}\}/,
  );
  assert.match(releaseWorkflow, /if \(\$hasCertificate -ne \$hasPassword\)/);
  assert.match(releaseWorkflow, /UAPI_WINDOWS_SIGNING=0/);
  assert.match(releaseWorkflow, /Import-PfxCertificate/);
  assert.match(releaseWorkflow, /1\.3\.6\.1\.5\.5\.7\.3\.3/);
  assert.match(releaseWorkflow, /- name: Sign staged Windows binaries/);
  assert.match(releaseWorkflow, /- name: Sign U-API Connect installer/);
  assert.match(releaseWorkflow, /signtool failed to verify the installer/);
  assert.match(releaseWorkflow, /Remove imported Authenticode certificate/);

  const signingStep = releaseWorkflow.indexOf("- name: Prepare optional Authenticode certificate");
  const windowsJob = releaseWorkflow.indexOf("  windows-installer:");
  assert.ok(signingStep > windowsJob, "signing secrets must be scoped to their preparation step");
  assert.doesNotMatch(
    releaseWorkflow.slice(windowsJob, signingStep),
    /secrets\.WINDOWS_CERTIFICATE/,
  );

  assert.match(windowsInstaller, /!ifdef SIGN_CERTIFICATE_THUMBPRINT/);
  assert.match(windowsInstaller, /!uninstfinalize[^\n]+\s= 0/);
  assert.match(windowsInstaller, /\$\{SIGNTOOL_PATH\}/);
  assert.match(windowsLifecycle, /Assert-AuthenticodeSignatures/);
  assert.match(windowsLifecycle, /UAPI_WINDOWS_SIGNING -ne "1"/);
  assert.match(windowsLifecycle, /Get-AuthenticodeSignature/);
});

test("release workflow publishes only validated U-API Connect assets", () => {
  assert.match(releaseWorkflow, /UAPI_CONNECT_DISTRIBUTION: "1"/);
  assert.match(releaseWorkflow, /push:\s*\n\s+tags:\s*\n\s+- "v\*-uapi\.\*"/);
  assert.doesNotMatch(releaseWorkflow, /release:\s*\n\s+types:\s*\[published\]/);
  assert.match(releaseWorkflow, /generate_release_notes: true/);
  assert.match(
    buildWorkflow.slice(0, buildWorkflow.indexOf("\njobs:")),
    /UAPI_CONNECT_DISTRIBUTION: "1"/,
  );
  assert.match(releaseWorkflow, /npm ci/);
  assert.match(releaseWorkflow, /cargo test --workspace --locked/);
  assert.match(releaseWorkflow, /cargo build --release --locked/);
  assert.match(releaseWorkflow, /UAPIConnect\.nsi/);
  assert.match(releaseWorkflow, /scripts\/uapi\/package-macos-dmg\.sh/);
  assert.match(releaseWorkflow, /UAPIConnect-\*-windows-x64-setup\.exe/);
  assert.match(releaseWorkflow, /UAPIConnect-\*-macos-\$\{\{ matrix\.arch \}\}\.dmg/);
  assert.match(releaseWorkflow, /SHA256SUMS/);
  assert.match(releaseWorkflow, /sha256sum --check SHA256SUMS/);
  assert.match(
    releaseWorkflow,
    /softprops\/action-gh-release@3d0d9888cb7fd7b750713d6e236d1fcb99157228 # v3\.0\.2/,
  );
  assert.match(releaseWorkflow, /tag_name: \$\{\{ github\.ref_name \}\}/);
  assert.match(releaseWorkflow, /overwrite_files: false/);
  assert.match(releaseWorkflow, /git merge-base --is-ancestor HEAD refs\/remotes\/origin\/main/);
  assert.match(releaseWorkflow, /files: dist\/release\/\*/);
  assert.doesNotMatch(releaseWorkflow, /CodexPlusPlus\.nsi/);
  assert.doesNotMatch(releaseWorkflow, /scripts\/installer\/macos\/package-dmg\.sh/);
  assert.doesNotMatch(releaseWorkflow, /npm install --package-lock=false/);
  assert.doesNotMatch(releaseWorkflow, /(?:path|files):[^\n]*CodexPlusPlus-/);
  assert.match(upstreamSurfaceAudit, /refs\/tags\/v\$\{base_version\}\^\{commit\}/);
  assert.doesNotMatch(upstreamSurfaceAudit, /git describe --tags/);
});

test("release jobs use immutable source and least-privilege publication", () => {
  const workflowHeader = releaseWorkflow.slice(0, releaseWorkflow.indexOf("\njobs:"));
  assert.match(workflowHeader, /permissions:\s*\n\s+contents: read/);
  assert.doesNotMatch(workflowHeader, /contents: write/);

  const publisher = releaseWorkflow.slice(releaseWorkflow.indexOf("  latest-json:"));
  assert.match(publisher, /permissions:\s*\n\s+actions: read\s*\n\s+contents: write/);

  const checkoutCount = (releaseWorkflow.match(/ref: \$\{\{ github\.sha \}\}/g) ?? []).length;
  assert.equal(checkoutCount, 3, "every release checkout must use the tag event commit SHA");
  const noCredentialCount = (releaseWorkflow.match(/persist-credentials: false/g) ?? []).length;
  assert.equal(noCredentialCount, 3, "release build jobs must not retain checkout credentials");

  assert.match(buildWorkflow, /windows:\s*\n\s+name: Windows x64\s*\n\s+needs: policy/);
  assert.match(buildWorkflow, /macos:\s*\n\s+name: macOS \$\{\{ matrix\.arch \}\}\s*\n\s+needs: policy/);

  for (const [name, workflow] of [
    ["build", buildWorkflow],
    ["release", releaseWorkflow],
  ]) {
    const actionRefs = [...workflow.matchAll(/uses:\s+([^#\s]+)/g)].map((match) => match[1]);
    assert.ok(actionRefs.length > 0, `${name} workflow must use audited actions`);
    for (const actionRef of actionRefs) {
      assert.match(
        actionRef,
        /@[0-9a-f]{40}$/,
        `${name} workflow action must be pinned to a full commit SHA: ${actionRef}`,
      );
    }
  }
});

test("Windows U-API builds pin and verify the NSIS compiler", () => {
  for (const [name, workflow] of [
    ["build", buildWorkflow],
    ["release", releaseWorkflow],
  ]) {
    assert.match(workflow, /- name: Install pinned NSIS/);
    assert.match(workflow, /choco install nsis[\s\S]*?--version=3\.12\.0/);
    assert.match(workflow, /--allow-downgrade/);
    assert.match(
      workflow,
      /--source=https:\/\/community\.chocolatey\.org\/api\/v2\//,
      `${name} workflow must use the official Chocolatey Community source`,
    );
    assert.match(
      workflow,
      /\$compilerVersion = \(& \$makensis \/VERSION 2>&1 \| Out-String\)\.Trim\(\)/,
    );
    assert.match(workflow, /\$compilerVersion -ne "v3\.12"/);
    assert.equal(
      (workflow.match(/\$compilerVersion = \(& \$makensis \/VERSION 2>&1 \| Out-String\)\.Trim\(\)/g) ?? [])
        .length,
      2,
      `${name} workflow must verify NSIS both after install and immediately before compilation`,
    );
    assert.doesNotMatch(workflow, /\$makensis\s*=\s*"makensis"/);
  }

  const nsisStep = releaseWorkflow.indexOf("- name: Install pinned NSIS");
  const signingStep = releaseWorkflow.indexOf("- name: Prepare optional Authenticode certificate");
  assert.ok(
    nsisStep >= 0 && nsisStep < signingStep,
    "NSIS must be verified before importing signing credentials",
  );
});

test("macOS U-API packages use the fixed distribution and valid bundle versions", () => {
  assert.match(localMacosBuild, /export UAPI_CONNECT_DISTRIBUTION=1/);
  for (const workflow of [buildWorkflow, releaseWorkflow]) {
    assert.match(workflow, /runner: macos-15-intel/);
    assert.match(workflow, /runner: macos-15\n\s+target: aarch64-apple-darwin/);
  }
  assert.match(macosPackager, /BUNDLE_VERSION="\$\{VERSION%%-\*\}"/);
  assert.match(macosPackager, /CFBundleVersion<\/key><string>\$BUNDLE_VERSION<\/string>/);
  assert.match(
    macosPackager,
    /CFBundleShortVersionString<\/key><string>\$BUNDLE_VERSION<\/string>/,
  );
  assert.match(macosPackager, /hdiutil verify "\$DMG"/);
  assert.doesNotMatch(
    macosPackager,
    /CFBundle(?:Short)?Version<\/key><string>\$VERSION<\/string>/,
  );
});

test("README is the U-API Connect install and safety entry point", () => {
  assert.match(readme, /^# U-API Connect/m);
  assert.match(readme, /https:\/\/github\.com\/BA7IEE\/UAPIConnect\/releases/);
  assert.match(readme, /Windows SmartScreen/);
  assert.match(readme, /是否带有签名取决于发布者是否配置了有效的代码签名证书/);
  assert.match(readme, /Get-AuthenticodeSignature/);
  assert.match(readme, /Evergreen bootstrapper/);
  assert.match(readme, /`WINDOWS_CERTIFICATE` 和 `WINDOWS_CERTIFICATE_PASSWORD`/);
  assert.match(readme, /已经合并到 `main` 的提交/);
  assert.match(readme, /--uninstall-cleanup/);
  assert.match(readme, /中止并保留程序文件/);
  assert.match(readme, /BigPizzaV3\/CodexPlusPlus/);
  assert.match(readme, /AGPL-3\.0-only/);

  const userGuide = readme.slice(0, readme.indexOf("## 上游与许可"));
  assert.doesNotMatch(userGuide, /BigPizzaV3\/CodexPlusPlus\/releases/);

  assert.match(windowsAcceptance, /UAPIConnect-<版本>-windows-x64-setup\.exe/);
  assert.match(windowsAcceptance, /`SHA256SUMS`/);
  assert.match(windowsAcceptance, /Get-AuthenticodeSignature/);
  assert.match(windowsAcceptance, /WebView2 缺失/);
  assert.doesNotMatch(windowsAcceptance, /SHA256SUMS\.txt/);
});
