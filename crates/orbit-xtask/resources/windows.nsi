Unicode true
RequestExecutionLevel user
Name "Orbit Debug"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\Orbit Debug"
InstallDirRegKey HKCU "Software\Orbit\DebugInstaller" "Directory"
SetCompressor zlib
CRCCheck force
Icon "${ASSETS}\orbit.ico"
UninstallIcon "${ASSETS}\orbit.ico"
!define ARP "Software\Microsoft\Windows\CurrentVersion\Uninstall\OrbitDebug"
!include "MUI2.nsh"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "Install"
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  File "${GUI_DIR}\orbit-gui.exe"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Orbit Debug"
  SetOutPath "$PROFILE"
  CreateShortcut "$SMPROGRAMS\Orbit Debug\Orbit Debug.lnk" "$INSTDIR\orbit-gui.exe"
  WriteRegStr HKCU "Software\Orbit\DebugInstaller" "Directory" "$INSTDIR"
  WriteRegStr HKCU "${ARP}" "DisplayName" "Orbit Debug"
  WriteRegStr HKCU "${ARP}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${ARP}" "Publisher" "Orbit"
  WriteRegStr HKCU "${ARP}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${ARP}" "DisplayIcon" "$INSTDIR\orbit-gui.exe"
  WriteRegStr HKCU "${ARP}" "UninstallString" '$\"$INSTDIR\uninstall.exe$\"'
  WriteRegStr HKCU "${ARP}" "QuietUninstallString" '$\"$INSTDIR\uninstall.exe$\" /S'
  WriteRegDWORD HKCU "${ARP}" "NoModify" 1
  WriteRegDWORD HKCU "${ARP}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\orbit-gui.exe"
  Delete "$INSTDIR\uninstall.exe"
  Delete "$SMPROGRAMS\Orbit Debug\Orbit Debug.lnk"
  RMDir "$SMPROGRAMS\Orbit Debug"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${ARP}"
  DeleteRegKey HKCU "Software\Orbit\DebugInstaller"
SectionEnd
