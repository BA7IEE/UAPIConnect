Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!define ROOT "..\..\..\.."

Name "U-API Connect"
OutFile "${ROOT}\dist\uapi\windows\UAPIConnect-${VERSION}-windows-x64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\U-API Connect"
InstallDirRegKey HKCU "Software\UAPIConnect" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

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

Section "Install"
  SetOutPath "$INSTDIR"
  nsExec::ExecToLog 'taskkill /IM codex-plus-plus.exe /F'
  Pop $0
  nsExec::ExecToLog 'taskkill /IM codex-plus-plus-manager.exe /F'
  Pop $0
  File "${ROOT}\dist\uapi\windows\app\codex-plus-plus.exe"
  File "${ROOT}\dist\uapi\windows\app\codex-plus-plus-manager.exe"
  CreateShortcut "$DESKTOP\U-API Connect.lnk" "$INSTDIR\codex-plus-plus.exe" "" "$INSTDIR\codex-plus-plus.exe"
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
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UAPIConnect" "UninstallString" "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  nsExec::ExecToLog 'taskkill /IM codex-plus-plus.exe /F'
  Pop $0
  nsExec::ExecToLog 'taskkill /IM codex-plus-plus-manager.exe /F'
  Pop $0
  Delete "$DESKTOP\U-API Connect.lnk"
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
SectionEnd
