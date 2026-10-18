# Windows installer template

`installer.nsi` is the template bundled in `@tauri-apps/cli-win32-x64-msvc`
2.11.2 (Tauri, MIT OR Apache-2.0; MIT notice in `LICENSE-TAURI`). It was copied
from the locally installed CLI package, without changing its installer flow.

The only customization is the current-user default directory:

```nsis
StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${PRODUCTNAME}"
```

This matches the stable installation path used by `kb/schedule.rs` and skill
installation. Tauri's unmodified default omits `Programs`. When upgrading the
Tauri CLI, refresh this template and reapply that one change; check the generated
NSIS script and rerun `tauriConfig.test.ts`.
