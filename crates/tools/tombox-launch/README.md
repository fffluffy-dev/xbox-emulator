# tombox-launch

Starts a dumped, already-decrypted Xbox GDK title with the runtime, on Windows or through Wine / Proton (Steam Deck).

```
tombox-launch <dump or Mount dir> --runtime <runtime DLL dir> [--work DIR] [--plan] [--run] [-- title args]
```

What it does:

1. Reads `MicrosoftGame.Config`. That gives the identity, version and the `Game` executable.
2. Scans every `.exe` / `.dll` of the package. It collects their imports, delay imports and DLL names loaded by string.
3. Sorts every DLL into a group:
   - **title**: the package ships it and the runtime has no replacement. Where the runtime builds a DLL of the same name, the runtime's copy wins. For example, the runtime's `XCurl.dll` sends every transfer through WinHTTP and the network guard, while the console XCurl the title ships crashes off the console. `--prefer-title NAME` keeps the title's copy;
   - **api set**: `api-ms-*` / `ext-ms-*`, which the host resolves and the runtime reroutes for console hosts;
   - **system**: the host provides it;
   - **runtime**: a console library (in `EmbeddedXvd/Windows/System32`, not on the host) that the runtime provides;
   - **missing**: a console library nobody provides.
4. Builds an overlay in `<work>/Mount`.
   - The package is mirrored with symlinks (Unix) or hard links (Windows), so the dump is never modified.
   - The **runtime** DLLs are copied beside the executable.
   - Any runtime DLL may forward to `<name>_sys.dll` (`advapi32legacy` to `advapi32_sys`, `mfplat` to `mfplat_sys`, ...). For each such name, a copy of the host's `<name>.dll` goes into the overlay under that name.
   - `--dxc DIR`, or `$TOMBOX_DXC_DIR`, points `dxcompiler_x` at Microsoft's `dxcompiler.dll` + `dxil.dll`. Without them the title's shader compiler calls fail.
5. Writes `<work>/launch_report.md` and `<work>/launch.env`.
   - The runtime environment: `TOMBOX_PACKAGE_ROOT`, `TOMBOX_XBTHUNK_TITLE_ROOT`, `TOMBOX_GDK_ROOT`, `TOMBOX_XCRD_ROOT`, `TOMBOX_COMPAT_DIR`, `TOMBOX_LOG`.
   - Off Windows also `WINEPATH`, so the runtime's own dependencies resolve after the host's system DLLs, and `WINEDLLOVERRIDES=<staged>=n,b`. These are appended to any existing overrides, so vkd3d-proton settings survive.
6. Without `--work`, everything goes to `~/tombox-run/<identity>` (`%LOCALAPPDATA%\tombox-run` on Windows). `--plan` reads only and also works before the runtime is built: console libraries then show as missing.
7. With `--run`, starts the title in the overlay: directly on Windows (optionally under `--mdbg mdbg.exe`), or with `--wine` / `$WINE` elsewhere (`--prefix` / `$WINEPREFIX` selects the prefix).

Wine has no Microsoft Gaming Runtime. The runtime's `xgameruntime.dll` (crate `crates/compat/xgameruntime`) stands in for it: its `QueryApiImpl` answers `E_NOINTERFACE`, and the compat-common GDK runtime, which hooks that entry point, serves every interface it emulates. On Windows the host's own `xgameruntime.dll` is used, and the stand-in is not staged. For a title that loads it from System32 only, `--into-prefix xgameruntime.dll` copies it into the Wine prefix. It never overwrites a different file.

The runtime DLLs need the MSVC linker. The `runtime-bundle` workflow builds them on GitHub Actions and uploads `tombox-runtime-windows-x64`, which also contains `tombox-launch.exe` and `mdbg.exe`.

Steam Deck example (Proton 10, the prefix the GPU tests use):

```
export WINEPREFIX=$HOME/xbe-prefix/pfx WINEDLLOVERRIDES="d3d12,d3d12core,dxgi,d3d11,d3d10core=n"
tombox-launch ~/Downloads/<package> --runtime ~/tombox-runtime --work ~/tombox-run/trackmania --plan
tombox-launch ~/Downloads/<package> --runtime ~/tombox-runtime --work ~/tombox-run/trackmania \
  --wine "$HOME/.steam/steam/steamapps/common/Proton 10.0/files/bin/wine64" --run
```

The overlay links point back into the dump. A console package is read-only, so a title should not write there. In case one does, the launcher records the size and modification time of every file in the dump before the run and compares them afterwards. It reports any change in `<work>/dump_changes.txt`. `--copy` isolates the dump completely, at the cost of disk space.

Use only dumps of games you own. The tool does no decryption.
