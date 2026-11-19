!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr SHCTX "Software\Classes\chat" "" "URL:Chat Protocol"
  WriteRegStr SHCTX "Software\Classes\chat" "URL Protocol" ""
  WriteRegStr SHCTX "Software\Classes\chat\shell\open\command" "" '$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ReadRegStr $0 SHCTX "Software\Classes\chat\shell\open\command" ""
  StrCmp $0 '$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\"' 0 +2
  DeleteRegKey SHCTX "Software\Classes\chat"
!macroend
