; The Smowaudio installer: per user, no admin rights, into %LOCALAPPDATA%\Smowaudio.
;
; Build: makensis /DVERSION=0.10.0 /DEXE=target\release\smowaudio.exe /DOUTFILE=Smowaudio_0.10.0_x64-setup.exe installer\smowaudio.nsi
;
; Updates run it as `/S /UPDATE /R /ARGS <the app's arguments>`: quietly, keeping shortcuts as they
; are, then starting the app again with the arguments it had. Versions up to 0.9.x were installed
; by Tauri's installer and update with these same arguments, so the paths, registry keys and
; shortcuts here match what it wrote.

Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "Pass /DVERSION=x.y.z"
!endif
!ifndef EXE
  !define EXE "..\target\release\smowaudio.exe"
!endif
!ifndef OUTFILE
  !define OUTFILE "Smowaudio_${VERSION}_x64-setup.exe"
!endif

; Overridable only to test the installer without touching a real install.
!ifndef PRODUCT
  !define PRODUCT "Smowaudio"
!endif
!define PUBLISHER "fi-smo"
!define BINARY "smowaudio.exe"
; Windows groups the running app's windows with its shortcuts by this id (set by the app too).
!define APP_ID "dev.fi-smo.smowaudio"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}"
!define PRODUCTKEY "Software\${PUBLISHER}\${PRODUCT}"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "WordFunc.nsh"

Name "${PRODUCT}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\${PRODUCT}"
InstallDirRegKey HKCU "${PRODUCTKEY}" ""
BrandingText "${PRODUCT} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "FileDescription" "${PRODUCT} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" ""

!define MUI_ICON "..\icons\icon.ico"
!define MUI_UNICON "..\icons\icon.ico"
!define MUI_ABORTWARNING

Var UpdateMode

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Start ${PRODUCT}"
!define MUI_FINISHPAGE_RUN_FUNCTION StartApp
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Add a desktop shortcut"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION DesktopShortcut
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; Waits for Smowaudio to exit (an update closes it just before starting this), and closes it if it
; doesn't: quietly during updates, after asking otherwise.
!macro CloseApp UN
Function ${UN}CloseApp
  StrCpy $1 0
  ${Do}
    nsExec::ExecToStack 'tasklist /FI "IMAGENAME eq ${BINARY}" /NH /FO CSV'
    Pop $0
    Pop $0
    ${WordFind} $0 "${BINARY}" "E+1{" $2
    ${If} ${Errors}
      Return
    ${EndIf}
    ${If} $1 = 10
      ${IfNot} ${Silent}
        MessageBox MB_OKCANCEL|MB_ICONINFORMATION "${PRODUCT} is running. Close it to continue?" /SD IDOK IDOK +2
        Abort
      ${EndIf}
      nsExec::Exec 'taskkill /IM "${BINARY}" /F'
      Pop $0
    ${EndIf}
    ${If} $1 > 40
      MessageBox MB_ICONSTOP "${PRODUCT} didn't close, so it can't be updated. Close it from the tray and try again." /SD IDOK
      Abort
    ${EndIf}
    IntOp $1 $1 + 1
    Sleep 250
  ${Loop}
FunctionEnd
!macroend
!insertmacro CloseApp ""
!insertmacro CloseApp "un."

; Gives a shortcut the app's id (System.AppUserModel.ID), so a pinned shortcut and the running
; window share one taskbar button. Standard NSIS has no command for it: this goes through the shell
; link's IPersistFile and IPropertyStore (vtable slots below) with the System plugin.
!define CLSID_ShellLink "{00021401-0000-0000-C000-000000000046}"
!define IID_IShellLinkW "{000214F9-0000-0000-C000-000000000046}"
!define IID_IPersistFile "{0000010B-0000-0000-C000-000000000046}"
!define IID_IPropertyStore "{886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99}"
!define PKEY_AppUserModel_ID "{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}"

Function SetAppId ; $R0 = shortcut path
  System::Call 'ole32::CoInitialize(p 0)'
  System::Call 'ole32::CoCreateInstance(g "${CLSID_ShellLink}", p 0, i 1, g "${IID_IShellLinkW}", *p .r1) i .r0'
  ${If} $0 = 0
    System::Call '$1->0(g "${IID_IPersistFile}", *p .r2) i .r0' ; QueryInterface
    ${If} $0 = 0
      System::Call '$2->5(w R0, i 2) i .r0' ; Load, read/write
      ${If} $0 = 0
        System::Call '$1->0(g "${IID_IPropertyStore}", *p .r3) i .r0'
        ${If} $0 = 0
          System::Call '*(g "${PKEY_AppUserModel_ID}", i 5) p .r4' ; PROPERTYKEY
          System::Call '*(&w64 "${APP_ID}") p .r5' ; the id's characters, inline
          System::Call '*(&i2 31, &i2 0, &i2 0, &i2 0, p r5, p 0) p .r6' ; PROPVARIANT, VT_LPWSTR
          System::Call '$3->6(p r4, p r6) i .r0' ; SetValue
          System::Call '$3->7() i .r0' ; Commit
          System::Call '$2->6(p 0, i 1) i .r0' ; Save
          System::Free $4
          System::Free $5
          System::Free $6
          System::Call '$3->2()' ; Release
        ${EndIf}
      ${EndIf}
      System::Call '$2->2()'
    ${EndIf}
    System::Call '$1->2()'
  ${EndIf}
  System::Call 'ole32::CoUninitialize()'
FunctionEnd

!macro Shortcut PATH
  CreateShortcut "${PATH}" "$INSTDIR\${BINARY}"
  StrCpy $R0 "${PATH}"
  Call SetAppId
!macroend

Function .onInit
  ${GetOptions} $CMDLINE "/UPDATE" $0
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
FunctionEnd

Section Install
  Call CloseApp
  SetOutPath $INSTDIR
  File "/oname=${BINARY}" "${EXE}"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "${PRODUCTKEY}" "" $INSTDIR
  WriteRegStr HKCU "${UNINSTKEY}" "MainBinaryName" "${BINARY}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayIcon" "$\"$INSTDIR\${BINARY}$\""
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTKEY}" "InstallLocation" "$\"$INSTDIR$\""
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr HKCU "${UNINSTKEY}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINSTKEY}" "EstimatedSize" "$0"

  ; Updates leave the shortcuts alone unless the Start menu one went missing.
  ${If} $UpdateMode <> 1
  ${OrIfNot} ${FileExists} "$SMPROGRAMS\${PRODUCT}.lnk"
    !insertmacro Shortcut "$SMPROGRAMS\${PRODUCT}.lnk"
  ${EndIf}
SectionEnd

Function .onInstSuccess
  ; After an update: start again the way it was running (`/R`, with the arguments after `/ARGS`).
  ${If} ${Silent}
    ${GetOptions} $CMDLINE "/R" $0
    ${IfNot} ${Errors}
      ${GetOptions} $CMDLINE "/ARGS" $0
      Exec '"$INSTDIR\${BINARY}" $0'
    ${EndIf}
  ${EndIf}
FunctionEnd

Function StartApp
  Exec '"$INSTDIR\${BINARY}"'
FunctionEnd

Function DesktopShortcut
  !insertmacro Shortcut "$DESKTOP\${PRODUCT}.lnk"
FunctionEnd

Function un.onInit
  ${GetOptions} $CMDLINE "/UPDATE" $0
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
FunctionEnd

Section Uninstall
  Call un.CloseApp
  Delete "$INSTDIR\${BINARY}"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  ${If} $UpdateMode <> 1
    Delete "$SMPROGRAMS\${PRODUCT}.lnk"
    Delete "$DESKTOP\${PRODUCT}.lnk"
    ; "Launch at Windows sign-in" would otherwise start an exe that's gone.
    nsExec::Exec 'schtasks /Delete /TN "${PRODUCT}" /F'
    Pop $0
    DeleteRegKey HKCU "${UNINSTKEY}"
    DeleteRegKey HKCU "${PRODUCTKEY}"
    DeleteRegKey /ifempty HKCU "Software\${PUBLISHER}"
  ${EndIf}
  ; Settings and the log stay in %APPDATA%\Smowaudio, for a later install.
SectionEnd
