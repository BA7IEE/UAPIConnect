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
  SetOutPath "$INSTDIR"
  nsExec::ExecToLog 'taskkill /IM codex-plus-plus.exe /F'
  Pop $0
  nsExec::ExecToLog 'taskkill /IM codex-plus-plus-manager.exe /F'
  Pop $0
  File "${ROOT}\dist\uapi\windows\app\codex-plus-plus.exe"
  File "${ROOT}\dist\uapi\windows\app\codex-plus-plus-manager.exe"
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
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegStr HKCU "Software\Classes\uapiconnect" "" "URL:U-API Connect Protocol"
  WriteRegStr HKCU "Software\Classes\uapiconnect" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\uapiconnect\shell\open\command" "" '"$INSTDIR\codex-plus-plus-manager.exe" "%1"'
SectionEnd

Section "Uninstall"
  nsExec::ExecToLog 'taskkill /IM codex-plus-plus.exe /F'
  Pop $0
  nsExec::ExecToLog 'taskkill /IM codex-plus-plus-manager.exe /F'
  Pop $0
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
  Delete "$INSTDIR\codex-plus-plus.exe"
  Delete "$INSTDIR\codex-plus-plus-manager.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect"
  DeleteRegKey HKCU "Software\UAPIConnect"
  DeleteRegKey HKCU "Software\Classes\uapiconnect"
SectionEnd
