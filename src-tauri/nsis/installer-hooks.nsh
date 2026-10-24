; installer-hooks.nsh — custom NSIS hooks for the K2K Packet Desktop installer.
;
; Uninstall semantics ("Delete app data" checkbox):
;   K2K keeps ALL app data (settings.json, settings.json.bak, pbbs_data.json,
;   history.json, logs/, WebView2 profile) NEXT TO THE EXECUTABLE
;   (portable-style since v0.9.47). The stock checkbox paths
;   (%APPDATA%\<BUNDLEID>, %LOCALAPPDATA%\<BUNDLEID>) therefore hold nothing,
;   which made the checkbox a no-op. When the checkbox IS checked, wipe the
;   entire install directory — the folder the user sees as "K2K Packet
;   Desktop" — which is where all the real data lives. Unchecked keeps the
;   current behavior: program files removed, data folder preserved.
!include "FileFunc.nsh"

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
    ; Safeguard: never recurse a drive root (e.g. "C:\") even if the app was
    ; installed directly onto one.
    ${GetRoot} "$INSTDIR" $0
    StrCmp "$0" "$INSTDIR" skip_root do_delete
    do_delete:
      RMDir /r "$INSTDIR"
    skip_root:
  ${EndIf}
!macroend
