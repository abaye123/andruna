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
;   RUNTIME        defined when the stages carry andruna-app.exe and the ART
;                  root (art\); the installer then prepares the runtime
;
; Per-user install into %LOCALAPPDATA%\Programs\Andruna without administrator
; rights (P-1). The installer picks the files that match the PC's processor (P-2),
; registers the Android package file types for the manager's install dialog
; (P-3) and an uninstaller under HKCU (P-4).

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
; The file type of Android packages (P-3); per user, in HKCU\Software\Classes.
!define PROGID "Andruna.Package"
!define CLASSES "Software\Classes"

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

; Ends Andruna processes of this user that run from the install folder (the
; manager, sysd and the apps it started), so their files can be replaced or
; removed. Processes of other users are not touched.
!macro StopAndruna
  nsExec::Exec '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "Get-Process -Name Andruna.Manager,andruna-sysd,andruna-launch,andruna-app -ErrorAction SilentlyContinue | Where-Object { $$_.Path -like \"$INSTDIR\*\" } | Stop-Process -Force"'
  Pop $0
!macroend

; Registers the package file types: the ProgID opens the manager's install
; dialog; each extension lists it under OpenWithProgids and gets it as its
; default only when no other program is the default.
!macro RegisterType EXT
  WriteRegStr HKCU "${CLASSES}\${EXT}\OpenWithProgids" "${PROGID}" ""
  ReadRegStr $0 HKCU "${CLASSES}\${EXT}" ""
  ${If} $0 == ""
    WriteRegStr HKCU "${CLASSES}\${EXT}" "" "${PROGID}"
  ${EndIf}
  WriteRegStr HKCU "${CLASSES}\Applications\${MANAGER_EXE}\SupportedTypes" "${EXT}" ""
!macroend

!macro UnregisterType EXT
  DeleteRegValue HKCU "${CLASSES}\${EXT}\OpenWithProgids" "${PROGID}"
  DeleteRegKey /ifempty HKCU "${CLASSES}\${EXT}\OpenWithProgids"
  ReadRegStr $0 HKCU "${CLASSES}\${EXT}" ""
  ${If} $0 == "${PROGID}"
    DeleteRegValue HKCU "${CLASSES}\${EXT}" ""
  ${EndIf}
  DeleteRegKey /ifempty HKCU "${CLASSES}\${EXT}"
!macroend

Section "Andruna"
  SetShellVarContext current
  !insertmacro StopAndruna
  ; The ART root is replaced as a whole, so that a boot image written for
  ; the previous runtime never stays next to the new one.
  RMDir /r "$INSTDIR\art"
  SetOutPath "$INSTDIR"

  ; Files of the matching architecture. Slots filled by build.ps1:
  ;   Andruna.Manager.exe and its runtime   (always)
  ;   andruna-sysd.exe, andruna-launch.exe
  ;   andruna-app.exe and art\              (RUNTIME: the guest ART root,
  ;                                          no boot image)
  ${If} ${IsNativeARM64}
    !ifdef STAGE_ARM64
      File /r "${STAGE_ARM64}\*.*"
    !endif
  ${Else}
    !ifdef STAGE_X64
      File /r "${STAGE_X64}\*.*"
    !endif
  ${EndIf}

  ; Run by the uninstaller before it removes this folder.
  File "uninstall-cleanup.ps1"
  WriteUninstaller "$INSTDIR\${UNINSTALLER}"
  CreateShortcut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\${MANAGER_EXE}"

  ; .apk, .apks, .xapk and .apkm open the install dialog (P-3).
  WriteRegStr HKCU "${CLASSES}\${PROGID}" "" "$(PackageFileType)"
  WriteRegStr HKCU "${CLASSES}\${PROGID}\DefaultIcon" "" "$INSTDIR\${MANAGER_EXE},0"
  WriteRegStr HKCU "${CLASSES}\${PROGID}\shell\open" "" "$(InstallVerb)"
  WriteRegStr HKCU "${CLASSES}\${PROGID}\shell\open\command" "" '"$INSTDIR\${MANAGER_EXE}" --install "%1"'
  WriteRegStr HKCU "${CLASSES}\Applications\${MANAGER_EXE}" "FriendlyAppName" "${PRODUCT}"
  WriteRegStr HKCU "${CLASSES}\Applications\${MANAGER_EXE}\shell\open\command" "" '"$INSTDIR\${MANAGER_EXE}" --install "%1"'
  !insertmacro RegisterType ".apk"
  !insertmacro RegisterType ".apks"
  !insertmacro RegisterType ".xapk"
  !insertmacro RegisterType ".apkm"
  ; SHCNE_ASSOCCHANGED: Explorer picks up the new file types.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'

  !ifdef RUNTIME
    ; The andrunaRuntime capability on the install folder (ADR 0036) and the
    ; boot image, written on this PC (ADR 0043). If this fails, sysd does it
    ; again in the background when it starts.
    DetailPrint "$(PreparingRuntime)"
    nsExec::Exec '"$INSTDIR\andruna-sysd.exe" --prepare-runtime'
    Pop $0
    ${If} $0 != 0
      DetailPrint "$(PrepareRuntimeFailed)"
    ${EndIf}
  !endif

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
  !insertmacro StopAndruna
  ; The manager removes its Windows notification registration.
  ${If} ${FileExists} "$INSTDIR\${MANAGER_EXE}"
    ExecWait '"$INSTDIR\${MANAGER_EXE}" --unregister'
  ${EndIf}
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  ; The Start menu shortcuts and "Installed apps" entries of Android apps
  ; (ADR 0027) start andruna-launch.exe from this folder; without it they
  ; would be broken, so they go with it (the apps' files and data stay
  ; unless the user removes them below). The manager's notification
  ; registration, which --unregister leaves, goes too.
  ${If} ${FileExists} "$INSTDIR\uninstall-cleanup.ps1"
    nsExec::Exec '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\uninstall-cleanup.ps1" -InstallDir "$INSTDIR"'
    Pop $0
  ${EndIf}
  RMDir /r "$INSTDIR"
  DeleteRegKey HKCU "${UNINST_KEY}"

  !insertmacro UnregisterType ".apk"
  !insertmacro UnregisterType ".apks"
  !insertmacro UnregisterType ".xapk"
  !insertmacro UnregisterType ".apkm"
  DeleteRegKey HKCU "${CLASSES}\${PROGID}"
  DeleteRegKey HKCU "${CLASSES}\Applications\${MANAGER_EXE}"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'

  ; Installed Android apps and their data live in %LOCALAPPDATA%\Andruna (P-4).
  ; They are kept unless the user asks to remove them; a silent uninstall keeps them.
  ${If} ${FileExists} "$LOCALAPPDATA\Andruna\*.*"
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "$(RemoveDataQuestion)" /SD IDNO IDNO keep_data
      RMDir /r "$LOCALAPPDATA\Andruna"
    keep_data:
  ${EndIf}
SectionEnd
