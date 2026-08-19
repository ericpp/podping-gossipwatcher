; Podping Gossip Watcher — Windows installer (NSIS)
; Bundles podping-gossipwatcher.exe and podping-gossipwatcher-tray.exe.

!include "MUI2.nsh"
!include "LogicLib.nsh"

!define PRODUCT_NAME "Podping Gossip Watcher"
!define PRODUCT_PUBLISHER "Podcast Index"
!define PRODUCT_WEB_SITE "https://github.com/Podcastindex-org/podping-gossipwatcher"
!define PRODUCT_UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"
!define PRODUCT_UNINST_ROOT "HKLM"

; Override at build time: makensis /DPRODUCT_VERSION=0.12.0 ...
!ifndef PRODUCT_VERSION
  !define PRODUCT_VERSION "0.12.0"
!endif

Name "${PRODUCT_NAME} ${PRODUCT_VERSION}"
OutFile "..\dist\PodpingGossipWatcher-${PRODUCT_VERSION}-setup.exe"
InstallDir "$PROGRAMFILES64\PodpingGossipWatcher"
InstallDirRegKey HKLM "${PRODUCT_UNINST_KEY}" "InstallLocation"
RequestExecutionLevel admin
Unicode True
ShowInstDetails show

!define MUI_ABORTWARNING
!define MUI_ICON "${NSISDIR}\Contrib\Graphics\Icons\modern-install.ico"
!define MUI_UNICON "${NSISDIR}\Contrib\Graphics\Icons\modern-uninstall.ico"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${PRODUCT_NAME} tray (minimized)"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchTray
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "MainSection" SEC01
  SetOutPath "$INSTDIR"

  File "..\target\release\podping-gossipwatcher.exe"
  File "..\target\release\podping-gossipwatcher-tray.exe"

  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME} Tray.lnk" \
    "$INSTDIR\podping-gossipwatcher-tray.exe" "--minimized"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall.lnk" "$INSTDIR\uninst.exe"

  WriteUninstaller "$INSTDIR\uninst.exe"

  WriteRegStr ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "URLInfoAbout" "${PRODUCT_WEB_SITE}"
  WriteRegStr ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "UninstallString" "$INSTDIR\uninst.exe"
  WriteRegStr ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "DisplayIcon" \
    "$INSTDIR\podping-gossipwatcher-tray.exe"
  WriteRegDWORD ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "NoModify" 1
  WriteRegDWORD ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}" "NoRepair" 1
SectionEnd

Function LaunchTray
  Exec '"$INSTDIR\podping-gossipwatcher-tray.exe" --minimized'
FunctionEnd

Section "Uninstall"
  Delete "$INSTDIR\podping-gossipwatcher.exe"
  Delete "$INSTDIR\podping-gossipwatcher-tray.exe"
  Delete "$INSTDIR\uninst.exe"

  Delete "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME} Tray.lnk"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall.lnk"
  RMDir "$SMPROGRAMS\${PRODUCT_NAME}"

  DeleteRegKey ${PRODUCT_UNINST_ROOT} "${PRODUCT_UNINST_KEY}"
  RMDir "$INSTDIR"
SectionEnd
