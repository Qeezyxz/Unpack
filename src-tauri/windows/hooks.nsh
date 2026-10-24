; Unpack installer hooks: Explorer right-click menu entries.
; Per-user install (no admin) -> everything goes under HKCU\Software\Classes.
; Windows 11 shows these under "Show more options" (classic menu).

!macro UNPACK_ADD_VERBS EXT
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.ExtractHere" "" "Extract here (Unpack)"
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.ExtractHere" "Icon" '"$INSTDIR\unpack.exe",0'
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.ExtractHere\command" "" '"$INSTDIR\unpack.exe" --extract-here -- "%1"'
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.ExtractFolder" "" "Extract to folder (Unpack)"
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.ExtractFolder" "Icon" '"$INSTDIR\unpack.exe",0'
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.ExtractFolder\command" "" '"$INSTDIR\unpack.exe" --extract-folder -- "%1"'
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.Open" "" "Open with Unpack"
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.Open" "Icon" '"$INSTDIR\unpack.exe",0'
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.Open\command" "" '"$INSTDIR\unpack.exe" -- "%1"'
!macroend

!macro UNPACK_DEL_VERBS EXT
  DeleteRegKey HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.ExtractHere"
  DeleteRegKey HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.ExtractFolder"
  DeleteRegKey HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell\Unpack.Open"
  DeleteRegKey /ifempty HKCU "Software\Classes\SystemFileAssociations\${EXT}\shell"
  DeleteRegKey /ifempty HKCU "Software\Classes\SystemFileAssociations\${EXT}"
!macroend

; --- File-association safety (fixes Tauri template gaps) ---
; Tauri's APP_ASSOCIATE backs up the old .7z/.rar handler into "<class>_backup".
; Gap 1: on a reinstall it would back up our OWN class and lose the real previous one.
; Gap 2: on uninstall with no previous HKCU handler it writes an EMPTY default value,
;        which hides a machine-wide (HKLM) 7-Zip/WinRAR association. We fix both here.
Var UnpackBak7z
Var UnpackBakRar

!macro UNPACK_FIX_BACKUP EXT CLASS VAR
  ReadRegStr $R0 HKCU "Software\Classes\.${EXT}" "${CLASS}_backup"
  ${If} $R0 == "${CLASS}"
    WriteRegStr HKCU "Software\Classes\.${EXT}" "${CLASS}_backup" "${VAR}"
  ${EndIf}
!macroend

!macro UNPACK_CLEAN_ASSOC EXT CLASS
  ReadRegStr $R0 HKCU "Software\Classes\.${EXT}" ""
  ${If} $R0 == ""
  ${OrIf} $R0 == "${CLASS}"
    DeleteRegValue HKCU "Software\Classes\.${EXT}" ""
  ${EndIf}
  DeleteRegValue HKCU "Software\Classes\.${EXT}" "${CLASS}_backup"
  DeleteRegKey /ifempty HKCU "Software\Classes\.${EXT}"
!macroend

!macro NSIS_HOOK_PREINSTALL
  ReadRegStr $UnpackBak7z HKCU "Software\Classes\.7z" "7-Zip archive_backup"
  ReadRegStr $UnpackBakRar HKCU "Software\Classes\.rar" "RAR archive_backup"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro UNPACK_CLEAN_ASSOC "7z" "7-Zip archive"
  !insertmacro UNPACK_CLEAN_ASSOC "rar" "RAR archive"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro UNPACK_FIX_BACKUP "7z" "7-Zip archive" "$UnpackBak7z"
  !insertmacro UNPACK_FIX_BACKUP "rar" "RAR archive" "$UnpackBakRar"
  !insertmacro UNPACK_ADD_VERBS ".zip"
  !insertmacro UNPACK_ADD_VERBS ".7z"
  !insertmacro UNPACK_ADD_VERBS ".rar"
  !insertmacro UNPACK_ADD_VERBS ".tar"
  !insertmacro UNPACK_ADD_VERBS ".gz"
  !insertmacro UNPACK_ADD_VERBS ".tgz"
  !insertmacro UNPACK_ADD_VERBS ".bz2"
  !insertmacro UNPACK_ADD_VERBS ".xz"
  ; Tell Explorer the menus changed (SHCNE_ASSOCCHANGED).
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro UNPACK_DEL_VERBS ".zip"
  !insertmacro UNPACK_DEL_VERBS ".7z"
  !insertmacro UNPACK_DEL_VERBS ".rar"
  !insertmacro UNPACK_DEL_VERBS ".tar"
  !insertmacro UNPACK_DEL_VERBS ".gz"
  !insertmacro UNPACK_DEL_VERBS ".tgz"
  !insertmacro UNPACK_DEL_VERBS ".bz2"
  !insertmacro UNPACK_DEL_VERBS ".xz"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend
