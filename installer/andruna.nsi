; Andruna installer (ADR 0017).
;
; Built by installer/build.ps1, which stages the files and writes a defines
; file passed as /DDEFINES=<path>, with:
;   VERSION        product version, a.b.c
;   OUTFILE        path of the installer to write
;   ICON           path of the .ico for installer and uninstaller
;   STAGE_X64      folder with the x64 files (optional)
;   STAGE_ARM64    folder with the ARM64 files (optional)
;   SIGN_CMD       signing command with %1 for the file (optional; unsigned without it)
;
; Per-user install into %LOCALAPPDATA%\Programs\Andruna without administrator
; rights (P-1). The installer picks the files that match the PC's processor (P-2)
; and registers an uninstaller under HKCU (P-4).

Unicode true
ManifestDPIAware true
ManifestSupportedOS Win10
RequestExecutionLevel user
; Only the zlib compression module: it is under the zlib/libpng license (ADR 0017).
SetCompressor /SOLID zlib

!ifdef DEFINES
  !include "${DEFINES}"
!endif

!include MUI2.nsh
!include LogicLib.nsh
!include x64.nsh
!include WinVer.nsh
!include FileFunc.nsh

!ifndef VERSION
  !error "VERSION is not defined; build with installer/build.ps1"
!endif
!ifndef STAGE_X64
  !ifndef STAGE_ARM64
    !error "neither STAGE_X64 nor STAGE_ARM64 is defined; build with installer/build.ps1"
  !endif
!endif

!define PRODUCT "Andruna"
!define MANAGER_EXE "Andruna.Manager.exe"
!define UNINSTALLER "Uninstall Andruna.exe"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Andruna"

!ifdef SIGN_CMD
  !uninstfinalize '${SIGN_CMD}' = 0
  !finalize '${SIGN_CMD}' = 0
!endif

Name "${PRODUCT}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Andruna"
ShowInstDetails nevershow
ShowUninstDetails nevershow
BrandingText " "

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=0 "ProductName" "${PRODUCT}"
VIAddVersionKey /LANG=0 "FileDescription" "${PRODUCT} Setup"
VIAddVersionKey /LANG=0 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "LegalCopyright" "Andruna contributors"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${MANAGER_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "$(FinishRun)"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; The first language is the fallback when Windows uses neither.
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Hebrew"
!include "lang\en-US.nsh"
!include "lang\he-IL.nsh"

Function .onInit
  ${IfNot} ${AtLeastBuild} 22000
    MessageBox MB_OK|MB_ICONSTOP "$(NeedsWindows11)" /SD IDOK
    Abort
  ${EndIf}

  ${If} ${IsNativeARM64}
    !ifndef STAGE_ARM64
      MessageBox MB_OK|MB_ICONSTOP "$(NoBuildForThisPC)" /SD IDOK
      Abort
    !endif
  ${ElseIf} ${IsNativeAMD64}
    !ifndef STAGE_X64
      MessageBox MB_OK|MB_ICONSTOP "$(NoBuildForThisPC)" /SD IDOK
      Abort
    !endif
  ${Else}
    MessageBox MB_OK|MB_ICONSTOP "$(NoBuildForThisPC)" /SD IDOK
    Abort
  ${EndIf}
FunctionEnd

Section "Andruna"
  SetShellVarContext current
  SetOutPath "$INSTDIR"

  ; Files of the matching architecture. Slots filled by build.ps1:
  ;   Andruna.Manager.exe and its runtime   (always)
  ;   andruna-sysd.exe                      (when built)
  ;   andruna-launch.exe                    (when built)
  ${If} ${IsNativeARM64}
    !ifdef STAGE_ARM64
      File /r "${STAGE_ARM64}\*.*"
    !endif
  ${Else}
    !ifdef STAGE_X64
      File /r "${STAGE_X64}\*.*"
    !endif
  ${EndIf}

  WriteUninstaller "$INSTDIR\${UNINSTALLER}"
  CreateShortcut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\${MANAGER_EXE}"

  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "Andruna contributors"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${MANAGER_EXE},0"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\${UNINSTALLER}"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\${UNINSTALLER}" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

Function un.onInit
  ; The uninstaller removes only the folder it was installed into.
  ${IfNot} ${FileExists} "$INSTDIR\${UNINSTALLER}"
    Abort
  ${EndIf}
FunctionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKCU "${UNINST_KEY}"

  ; Installed Android apps and their data live in %LOCALAPPDATA%\Andruna (P-4).
  ; They are kept unless the user asks to remove them; a silent uninstall keeps them.
  ${If} ${FileExists} "$LOCALAPPDATA\Andruna\*.*"
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "$(RemoveDataQuestion)" /SD IDNO IDNO keep_data
      RMDir /r "$LOCALAPPDATA\Andruna"
    keep_data:
  ${EndIf}
SectionEnd
