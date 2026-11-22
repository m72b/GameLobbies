; Installer hooks (bundle > windows > nsis > installerHooks in tauri.conf.json).
;
; Moves an install of the app made under its old name, "Game Lobby" (0.9.0 and older), over to
; this one. The NSIS installer keys an install by the product name (uninstall entry, install
; folder, shortcuts, the Run value it removes), so without this a GameLobbies installer, run by
; hand or by the in-app updater, would install next to the old one and leave it behind.
;
; The library is not touched: it lives in %APPDATA%\${BUNDLEID}, which is named by the app
; identifier and did not change. The old uninstaller is run silently in update mode (/S /UPDATE),
; and its template deletes app data only when its "delete app data" box was ticked and it is not
; in update mode; with /S that box is never shown.
;
; Safe to run more than once: the old uninstaller removes the old uninstall entry, so a second run
; finds nothing to do. Nothing here runs when no old install is found.
;
; This file is included before the template defines PRODUCTNAME and the other names, so the old
; names are written out here and the template's names are used only inside the macros.

!define GL_OLD_PRODUCTNAME "Game Lobby"
!define GL_OLD_PUBLISHER "gamelobby"
!define GL_OLD_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Game Lobby"
!define GL_OLD_MANUPRODUCTKEY "Software\gamelobby\Game Lobby"
!define GL_RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

; 1 when an old install was found and removed by this installer.
Var GlMigrated
; The old install folder and main binary name ("game-lobby.exe").
Var GlOldDir
Var GlOldBinary
; 1 when the old Start menu / desktop shortcut pointed at the old app and was removed.
Var GlOldStartMenu
Var GlOldDesktop

!macro NSIS_HOOK_PREINSTALL
  StrCpy $GlMigrated 0
  StrCpy $GlOldStartMenu 0
  StrCpy $GlOldDesktop 0

  ; Only this app's own old install: same uninstall entry name and the same publisher (derived
  ; from the unchanged identifier). The old install was always per user (HKCU).
  ReadRegStr $R0 HKCU "${GL_OLD_UNINSTKEY}" "DisplayName"
  ReadRegStr $R1 HKCU "${GL_OLD_UNINSTKEY}" "Publisher"
  ${If} $R0 == "${GL_OLD_PRODUCTNAME}"
  ${AndIf} $R1 == "${GL_OLD_PUBLISHER}"
    ; The folder the old installer recorded; else the uninstall entry's InstallLocation.
    ReadRegStr $GlOldDir HKCU "${GL_OLD_MANUPRODUCTKEY}" ""
    ${If} $GlOldDir == ""
      ReadRegStr $GlOldDir HKCU "${GL_OLD_UNINSTKEY}" "InstallLocation"
      ; Written quoted: "C:\...\Game Lobby"
      StrCpy $R2 $GlOldDir 1
      ${If} $R2 == '"'
        StrCpy $GlOldDir $GlOldDir "" 1
        StrCpy $GlOldDir $GlOldDir -1
      ${EndIf}
    ${EndIf}
    ReadRegStr $GlOldBinary HKCU "${GL_OLD_UNINSTKEY}" "MainBinaryName"
    ${If} $GlOldBinary == ""
      StrCpy $GlOldBinary "game-lobby.exe"
    ${EndIf}

    ${If} $GlOldDir != ""
      DetailPrint "Moving ${GL_OLD_PRODUCTNAME} from $GlOldDir to ${PRODUCTNAME}. Your library and settings are kept."
      ; Close the old app if it runs (asks first unless silent or passive), like the template
      ; does for this app's own path.
      !insertmacro CheckIfAppIsRunning "$GlOldDir\$GlOldBinary" "${GL_OLD_PRODUCTNAME}"

      ${If} ${FileExists} "$GlOldDir\uninstall.exe"
        ; In place (_?=) so ExecWait waits for it; update mode keeps app data, the Run value
        ; and the shortcuts (removed below only when they point at the old app).
        ClearErrors
        ExecWait '"$GlOldDir\uninstall.exe" /S /UPDATE _?=$GlOldDir' $R3
        ${IfThen} ${Errors} ${|} StrCpy $R3 2 ${|}
      ${Else}
        ; The folder is gone already; only stale entries are left.
        StrCpy $R3 0
      ${EndIf}

      ${If} $R3 = 0
        StrCpy $GlMigrated 1
        ; The uninstaller ran in place, so it could not remove itself or its folder. RMDir
        ; without /r removes the folder only if it is empty now.
        Delete "$GlOldDir\uninstall.exe"
        ${If} $GlOldDir != $INSTDIR
          RMDir "$GlOldDir"
        ${EndIf}
        DeleteRegKey HKCU "${GL_OLD_UNINSTKEY}"
        DeleteRegKey HKCU "${GL_OLD_MANUPRODUCTKEY}"
        DeleteRegKey /ifempty HKCU "Software\gamelobby"

        ; Old shortcuts, only those that start the old app.
        !insertmacro IsShortcutTarget "$SMPROGRAMS\${GL_OLD_PRODUCTNAME}.lnk" "$GlOldDir\$GlOldBinary"
        Pop $R4
        ${If} $R4 = 1
          StrCpy $GlOldStartMenu 1
          !insertmacro UnpinShortcut "$SMPROGRAMS\${GL_OLD_PRODUCTNAME}.lnk"
          Delete "$SMPROGRAMS\${GL_OLD_PRODUCTNAME}.lnk"
        ${EndIf}
        !insertmacro IsShortcutTarget "$DESKTOP\${GL_OLD_PRODUCTNAME}.lnk" "$GlOldDir\$GlOldBinary"
        Pop $R4
        ${If} $R4 = 1
          StrCpy $GlOldDesktop 1
          !insertmacro UnpinShortcut "$DESKTOP\${GL_OLD_PRODUCTNAME}.lnk"
          Delete "$DESKTOP\${GL_OLD_PRODUCTNAME}.lnk"
        ${EndIf}
      ${Else}
        ; Leave the old install as it is; this one is installed next to it.
        DetailPrint "Could not remove ${GL_OLD_PRODUCTNAME} (exit code $R3). It is left installed; uninstall it from Windows Settings."
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ${If} $GlMigrated = 1
    ; An update (/UPDATE) creates no shortcuts, so put back the ones the old install had. A
    ; normal install creates them itself (Start menu always, desktop from the finish page).
    ${If} $UpdateMode = 1
    ${AndIf} $NoShortcutMode <> 1
      ${If} $GlOldStartMenu = 1
      ${AndIfNot} ${FileExists} "$SMPROGRAMS\${PRODUCTNAME}.lnk"
        CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
        !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      ${EndIf}
      ${If} $GlOldDesktop = 1
      ${AndIfNot} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
        CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
        !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
      ${EndIf}
    ${EndIf}

    ; "Launch on boot": the old Run value named the old exe. Point the new value at this one, in
    ; the app's own format (autostart.rs), so Windows starts the app even before it runs once.
    ; The app also renames the value at startup and removes it if the setting is off.
    ReadRegStr $R0 HKCU "${GL_RUN_KEY}" "${GL_OLD_PRODUCTNAME}"
    ${If} $R0 != ""
      WriteRegStr HKCU "${GL_RUN_KEY}" "${PRODUCTNAME}" '"$INSTDIR\${MAINBINARYNAME}.exe" --autostart'
      DeleteRegValue HKCU "${GL_RUN_KEY}" "${GL_OLD_PRODUCTNAME}"
    ${EndIf}
  ${EndIf}
!macroend
