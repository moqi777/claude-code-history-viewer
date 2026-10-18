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

## Console companion for skills

The Windows configuration enables Cargo's `windows-cli` feature. Tauri builds
and bundles both `AgentKB.exe` (GUI subsystem) and `agentkb-cli.exe` (console
subsystem), and its existing `binaries` loop installs/uninstalls the companion.
No extra installer-template modification or manually copied sidecar is needed.

`cli.rs` calls the same KB dispatcher for search, show and skill-print; it never
starts Tauri or performs scheduled collection. Windows skill template 6 points
at this companion. Existing skills show as needing an update; users update them
from Settings after upgrading the app. A missing companion blocks skill install
with an instruction to repair the app installation.

The binary is declared with `required-features` and lives outside `src/bin`:
Tauri 2.11.2's auto-discovery of that directory would bypass the feature filter
when collecting binaries for a Mac bundle. macOS keeps template 5 and its
existing app executable. Build Windows packages normally with `pnpm tauri build`;
run Cargo CLI tests with `--features windows-cli` (or `--all-features`).
