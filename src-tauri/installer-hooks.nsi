; NSIS hooks for the OpenFan installer.
;
; OpenFan is two programs. The window is an ordinary unelevated application; the fan
; control itself is a Windows service that starts at boot, holds the elevated handle to
; the hardware, and keeps running when the window is closed. The installer is the only
; place with the rights to register that service, so it does.
;
; Ordering here is a safety property, not tidiness:
;
;   * On uninstall the service is **stopped before anything is deleted**. Stopping it runs
;     the dying breath, which hands every channel back to the board's own fan curve.
;     Deleting the binary out from under a running service would leave the fans wherever
;     OpenFan last set them, with nothing responding to temperature.
;   * On upgrade the old service is stopped before files are replaced, for the same reason
;     and because Windows will not overwrite a running executable.

!macro NSIS_HOOK_PREINSTALL
  ; An upgrade over a running installation: stand the old service down first, both so its
  ; channels go back to firmware and so its binary is not locked.
  DetailPrint "Stopping any running OpenFan service..."
  nsExec::ExecToLog '"$SYSDIR\sc.exe" stop OpenFan'
  Pop $0
  Sleep 2000
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Registering the OpenFan fan control service..."
  ; The service registers itself: the executable knows its own name, description and
  ; start type, so there is one definition of those rather than two that can disagree.
  nsExec::ExecToLog '"$INSTDIR\openfan-service.exe" --install'
  Pop $0
  ${If} $0 != 0
    DetailPrint "The service could not be registered (code $0)."
    DetailPrint "OpenFan will run, but cannot control fans until it is."
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Stop *and* remove before deleting files. Stopping applies the dying breath; removing
  ; takes the service out of the SCM so a later install starts clean.
  DetailPrint "Stopping and removing the OpenFan fan control service..."
  nsExec::ExecToLog '"$INSTDIR\openfan-service.exe" --uninstall'
  Pop $0
  Sleep 2000
!macroend
