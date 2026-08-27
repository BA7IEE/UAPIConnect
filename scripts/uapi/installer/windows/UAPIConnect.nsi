Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef WEBVIEW2_BOOTSTRAPPER
  !error "WEBVIEW2_BOOTSTRAPPER must point to the verified Microsoft Evergreen bootstrapper"
!endif
!define ROOT "..\..\..\.."
!define WEBVIEW2_APP_GUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

!ifndef SIGN_TIMESTAMP_URL
  !define SIGN_TIMESTAMP_URL "http://timestamp.digicert.com"
!endif

Name "U-API Connect"
OutFile "${ROOT}\dist\uapi\windows\UAPIConnect-${VERSION}-windows-x64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\U-API Connect"
InstallDirRegKey HKCU "Software\UAPIConnect" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!ifdef SIGN_CERTIFICATE_THUMBPRINT
  !ifndef SIGNTOOL_PATH
    !error "SIGNTOOL_PATH is required when Authenticode signing is enabled"
  !endif
  !uninstfinalize '"${SIGNTOOL_PATH}" sign /fd SHA256 /sha1 "${SIGN_CERTIFICATE_THUMBPRINT}" /d "U-API Connect" /tr "${SIGN_TIMESTAMP_URL}" /td SHA256 "%1"' = 0
!endif

!define MUI_ICON "${ROOT}\apps\codex-plus-manager\src-tauri\icons\icon.ico"
!define MUI_UNICON "${ROOT}\apps\codex-plus-manager\src-tauri\icons\icon.ico"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Function GetWebView2Version
  StrCpy $0 ""
  ; NSIS runs as a 32-bit process. Select the logical 32-bit view instead of
  ; spelling the reserved WOW6432Node physical path, which would be redirected again.
  SetRegView 32
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_APP_GUID}" "pv"

  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_APP_GUID}" "pv"
  ${EndIf}
  SetRegView Default
  Push $0
FunctionEnd

Section "-WebView2 Runtime"
  Call GetWebView2Version
  Pop $0
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    Goto webview2_done
  ${EndIf}

  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=MicrosoftEdgeWebView2Setup.exe "${WEBVIEW2_BOOTSTRAPPER}"
  DetailPrint "正在安装 Microsoft Edge WebView2 Runtime..."
  ClearErrors
  ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebView2Setup.exe" /silent /install' $1
  IfErrors webview2_exec_failed
  Delete "$PLUGINSDIR\MicrosoftEdgeWebView2Setup.exe"
  ${If} $1 != 0
    Goto webview2_failed
  ${EndIf}

  Call GetWebView2Version
  Pop $0
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    Goto webview2_failed
  ${EndIf}
  Goto webview2_done

webview2_exec_failed:
  Delete "$PLUGINSDIR\MicrosoftEdgeWebView2Setup.exe"
  Goto webview2_failed

webview2_failed:
  DetailPrint "Microsoft Edge WebView2 Runtime 安装失败，U-API Connect 尚未写入。"
  SetErrorLevel 2
  IfSilent webview2_abort
  MessageBox MB_ICONSTOP|MB_OK "无法安装 Microsoft Edge WebView2 Runtime。请检查网络连接，或先从微软官方渠道安装或修复 WebView2 后重试。"
webview2_abort:
  Abort

webview2_done:
SectionEnd

Section "Install"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=stop-owned-processes.ps1 "${ROOT}\scripts\uapi\installer\windows\stop-owned-processes.ps1"
  DetailPrint "正在停止此安装目录中的 U-API Connect 进程..."
  ClearErrors
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\stop-owned-processes.ps1" -InstallDir "$INSTDIR"' $0
  IfErrors install_stop_failed
  StrCmp $0 "0" install_processes_stopped install_stop_failed

install_stop_failed:
  Delete "$PLUGINSDIR\stop-owned-processes.ps1"
  DetailPrint "无法安全停止此安装目录中的 U-API Connect 进程，尚未写入程序文件。"
  SetErrorLevel 2
  IfSilent install_stop_aborted
  MessageBox MB_ICONSTOP|MB_OK "无法安全停止当前安装目录中的 U-API Connect。安装已中止，其他目录中的同名程序不会受到影响。请关闭本目录中的 U-API Connect 后重试。"
install_stop_aborted:
  Abort

install_processes_stopped:
  Delete "$PLUGINSDIR\stop-owned-processes.ps1"
  SetOutPath "$INSTDIR"
  File "${ROOT}\dist\uapi\windows\app\codex-plus-plus.exe"
  File "${ROOT}\dist\uapi\windows\app\codex-plus-plus-manager.exe"
  File /oname=quiet-uninstall-bootstrap.ps1 "${ROOT}\scripts\uapi\installer\windows\quiet-uninstall-bootstrap.ps1"
  CreateShortcut "$DESKTOP\U-API Connect.lnk" "$INSTDIR\codex-plus-plus.exe" "" "$INSTDIR\codex-plus-plus.exe"
  CreateShortcut "$DESKTOP\U-API Connect 设置.lnk" "$INSTDIR\codex-plus-plus-manager.exe" "" "$INSTDIR\codex-plus-plus-manager.exe"
  CreateDirectory "$SMPROGRAMS\U-API Connect"
  CreateShortcut "$SMPROGRAMS\U-API Connect\U-API Connect.lnk" "$INSTDIR\codex-plus-plus.exe" "" "$INSTDIR\codex-plus-plus.exe"
  CreateShortcut "$SMPROGRAMS\U-API Connect\U-API Connect 设置.lnk" "$INSTDIR\codex-plus-plus-manager.exe" "" "$INSTDIR\codex-plus-plus-manager.exe"
  CreateShortcut "$SMPROGRAMS\U-API Connect\卸载 U-API Connect.lnk" "$INSTDIR\uninstall.exe"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\UAPIConnect" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "DisplayName" "U-API Connect"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "Publisher" "U-Studio"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "DisplayIcon" "$INSTDIR\codex-plus-plus-manager.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "UninstallString" '"$INSTDIR\uninstall.exe"'
  ; A normal NSIS uninstaller self-copies, so its real SetErrorLevel value is
  ; hidden from the registered command's caller. The bootstrap copies it first,
  ; invokes that copy with _?=$INSTDIR, waits, and returns the real exit code.
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "QuietUninstallString" '"$WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "$INSTDIR\quiet-uninstall-bootstrap.ps1" -InstallDir "$INSTDIR"'
  WriteRegStr HKCU "Software\Classes\uapiconnect" "" "URL:U-API Connect Protocol"
  WriteRegStr HKCU "Software\Classes\uapiconnect" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\uapiconnect\shell\open\command" "" '"$INSTDIR\codex-plus-plus-manager.exe" "%1"'
SectionEnd

Section "Uninstall"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=stop-owned-processes.ps1 "${ROOT}\scripts\uapi\installer\windows\stop-owned-processes.ps1"
  DetailPrint "正在停止此安装目录中的 U-API Connect 进程..."
  ClearErrors
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\stop-owned-processes.ps1" -InstallDir "$INSTDIR"' $0
  IfErrors uninstall_stop_failed
  StrCmp $0 "0" uninstall_processes_stopped uninstall_stop_failed

uninstall_stop_failed:
  Delete "$PLUGINSDIR\stop-owned-processes.ps1"
  DetailPrint "无法安全停止此安装目录中的 U-API Connect 进程，已中止卸载。"
  SetErrorLevel 2
  IfSilent uninstall_stop_aborted
  MessageBox MB_ICONSTOP|MB_OK "无法安全停止当前安装目录中的 U-API Connect。卸载已中止，程序文件仍然保留，其他目录中的同名程序不会受到影响。"
uninstall_stop_aborted:
  Abort

uninstall_processes_stopped:
  Delete "$PLUGINSDIR\stop-owned-processes.ps1"
  DetailPrint "正在解除 U-API Connect 对 Codex 的接管并清理自有凭据..."
  ClearErrors
  ExecWait '"$INSTDIR\codex-plus-plus-manager.exe" --uninstall-cleanup' $0
  IfErrors cleanup_failed
  StrCmp $0 "0" cleanup_succeeded cleanup_failed

cleanup_failed:
  DetailPrint "清理未完成，已中止卸载并保留程序文件。"
  SetErrorLevel 2
  IfSilent cleanup_aborted
  MessageBox MB_ICONSTOP|MB_OK "无法安全清理 U-API Connect 的连接配置与凭据。卸载已中止，程序文件仍然保留。请重新打开 U-API Connect 设置后再试。"
cleanup_aborted:
  Abort

cleanup_succeeded:
  Delete "$DESKTOP\U-API Connect.lnk"
  Delete "$DESKTOP\U-API Connect 设置.lnk"
  Delete "$SMPROGRAMS\U-API Connect\U-API Connect.lnk"
  Delete "$SMPROGRAMS\U-API Connect\U-API Connect 设置.lnk"
  Delete "$SMPROGRAMS\U-API Connect\卸载 U-API Connect.lnk"
  RMDir "$SMPROGRAMS\U-API Connect"
  Delete "$INSTDIR\quiet-uninstall-bootstrap.ps1"
  Delete "$INSTDIR\codex-plus-plus.exe"
  Delete "$INSTDIR\codex-plus-plus-manager.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect"
  DeleteRegKey HKCU "Software\UAPIConnect"
  DeleteRegKey HKCU "Software\Classes\uapiconnect"
SectionEnd
