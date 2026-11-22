# Handoff (2026-11-22)

State of the work at the end of the Claude Code session, written for the next agent. It covers what exists, what is open, how to test, and what to do next.

## Repository and rules

- **Fork:** `fffluffy-dev/xbox-emulator`, project name **fucking-frankenstein** (README). **Upstream:** `omonkeyz/xbox-emulator`, merged into the fork at `1e77fcc` (PR #27). Upstream is an ancestor of the fork now, so a later sync is `git fetch upstream && git merge upstream/main`.
- **Workflow:** one problem, one issue, one branch, one PR. All of it happens in the fork; no PRs go to upstream.
- **Legal:** use only dumps of games the user owns. Never download dumps (for example from archive.org) and never decrypt anything.
- **Code style:** upstream's PR gate (`.github/scripts/pr_gate.py`, workflow `pr-gate.yml`) rejects comment lines (`//`, `///`, `#`) in added `.rs` / `.py` / `.ps1` lines. Run `python3 -I .github/scripts/pr_gate.py --base origin/main --head HEAD` before pushing.
- **Known red CI:** `rust-workspace-check` fails on every branch. It is issue #14: `wldp` and `gamecorecomproxy` `include_bytes!` data blobs that are not in the repository. It is `continue-on-error` and not caused by your PR. The check that matters is **`rust-core`** (Windows, D3D12 WARP, runs the tombox-xshader GPU tests).

## How to test without the user's PC

1. **Linux container:**
   - `cargo test -p tombox-xshader`, `cargo test -p tombox-launch` and `cargo test -p xgameruntime` all work.
   - Windows-only code: `cargo check -p <crate> --tests --target x86_64-pc-windows-gnu`. There is no mingw linker, so check only, no link.
2. **GitHub Actions:**
   - `rust.yml` (`rust-core`) runs the tombox-xshader tests on WARP.
   - `runtime-bundle.yml` builds the **MSVC release runtime DLLs**, `mdbg.exe` and `tombox-launch.exe`, and uploads `tombox-runtime-windows-x64`.
     - It runs on pushes to `main` and `claude/tombox-launch`, and on `workflow_dispatch`.
     - `MISSING.txt` in the bundle lists any core DLL that did not build.
3. **Steam Deck** (real RDNA2, SteamOS, Proton 10 / vkd3d-proton / RADV). The user's machine, used through the user.
   - Repo checkout: `~/xbox-emulator`. Builds run in the distrobox `xbe` (`distrobox enter xbe -- bash -lc '...'`).
   - Environment for GPU tests and title runs:
     ```
     D="$HOME/.steam/steam/steamapps/common/Proton 10.0/files"
     export WINEPREFIX=$HOME/xbe-prefix/pfx WINEDEBUG=-all \
       WINEDLLOVERRIDES="d3d12,d3d12core,dxgi,d3d11,d3d10core=n" VKD3D_DEBUG=none \
       TOMBOX_DXC_DIR='Z:\home\deck\xbe-dxc\bin\x64' DISABLE_MANGOHUD=1 MANGOHUD=0 TOMBOX_EXEC_HW=1
     E=$(ls -t ~/xbox-emulator/target/x86_64-pc-windows-gnu/debug/build/tombox-xshader/*/out/exec-*.exe | head -1)
     "$D/bin/wine64" $E <filter> --test-threads=1
     ```
   - Logs go to `~/xbe-logs/`.

## Done in this session (all merged into `main`)

- **Docs:** README and `docs/ROADMAP.md` rewritten, project renamed (#26).
- **Translator PRs, all merged:**
  - #1: portable tests build on Linux.
  - #2: MIMG TFE status VGPR.
  - #6: image atomics.
  - #8: GCN SMRD `s_memtime`.
  - #10: fuzz fixes.
  - #12: `live_in` store data.
  - #16: VS inputs from the FSSM table.
  - #19: hw-test assumptions.
  - #21: round-mode fold on RADV.
  - #23: mesh wave size from the FSSM header.
- **Memory-descriptor mode for shaders without a root signature** (#25; GTA VI resource model ported from Leonida):
  - step 1, buffers: verified on the Deck and on WARP;
  - step 2, images and samplers through a T#/S# lookup table and `ResourceDescriptorHeap`: verified on the Deck and on WARP.
- **Upstream sync** (#27). Where both sides did the same thing:
  - image atomics: upstream's `atomic_rmw` kept;
  - TFE: the fork's status VGPR kept;
  - shaders without a root signature: the memdesc fallback runs after upstream's user-data / inference modes.
- **19 RDNA2 mnemonics marked verified** (787/794): f16 FMAs, packed shifts, image atomics, GWS counters.
- **f64 division on RADV** (#28): `v_div_scale_f64` without denormal divisions; verified on the Deck.

## Open PRs

- **#30 `claude/tombox-launch`** — the title launcher, plus `runtime-bundle.yml` and `crates/compat/xgameruntime`.
  - `crates/tools/tombox-launch` (README there) reads `MicrosoftGame.Config`, scans imports, builds an overlay of the dump (links, so the dump is never written) and stages runtime DLLs beside the exe.
  - It sets `TOMBOX_*` (and `WINEPATH` / `WINEDLLOVERRIDES` under Wine) and starts the title natively or through Proton.
  - Options: `--plan`, `--run`, `--dxc DIR`, `--into-prefix NAME`, `--prefer-title NAME`, `--stage NAME`, `--mdbg`, `--copy`.
  - It snapshots the dump and reports any write into it.
  - `crates/compat/xgameruntime` is a stand-in Gaming Runtime for Wine: five exports, `QueryApiImpl` returns `E_NOINTERFACE`.
  - The head (`cce03ff`) also contains #31.
- **#31 `claude/gameinput-fallback`**: when the real `GameInputCreate` fails (Wine), compat-common returns a built-in `IGameInput` with no host devices, plus the virtual Xbox pad.
  - Verified on the Deck: it gets past `GameInputCreate`.
- Merge #31 and #30 once the Trackmania investigation below no longer needs changes in them, or merge now; both are self-contained.

## Current investigation: Trackmania on the Steam Deck

The title is the user's own copy, `~/Downloads/Ubisoft.TrackmaniaNext_1.6.2.0_neutral__ngz4m417e0mpw`: Xbox One GDK, 1.6.2.0, `GbxGame.exe`, console OS in `EmbeddedXvd`. The runtime bundle from CI run 45075789837 (`cce03ff`) is unpacked in `~/tombox-runtime3`.

Run command (Deck):
```
D="$HOME/.steam/steam/steamapps/common/Proton 10.0/files"
export WINEPREFIX=$HOME/xbe-prefix/pfx WINEDLLOVERRIDES="d3d12,d3d12core,dxgi,d3d11,d3d10core=n" DISABLE_MANGOHUD=1 MANGOHUD=0
cd ~/xbox-emulator && timeout 240 target/release/tombox-launch ~/Downloads/Ubisoft.TrackmaniaNext_1.6.2.0_neutral__ngz4m417e0mpw \
  --runtime ~/tombox-runtime3 --prefix $WINEPREFIX --work ~/tombox-run/trackmania --dxc ~/xbe-dxc/bin/x64 \
  --wine "$D/bin/wine64" --run > ~/xbe-logs/tm_runN.txt 2>&1; "$D/bin/wineserver" -k
```
The runtime log is `~/tombox-run/trackmania/logs/tombox_compat.log`. It appends, so move it aside before each run.

Progress across runs 1 to 4:
1. Crash in the title's console `XCurl.dll`. Fixed: the runtime's `xcurl.dll` (WinHTTP) now wins over the title's copy.
2. DXC missing, so `dxcompiler_x` returned `E_NOTIMPL`; `advapi32_sys.dll` was also missing. Fixed: `--dxc` and companion staging.
3. Got far. The runtime GDK came up with 36 interfaces; the `G:` drive is on the overlay; net guard and XCurl on WinHTTP; D3D12 device through vkd3d-proton; queues, heaps, resources, xg_x tiling; 8 shaders translated; ~40 compute PSOs; 3 `ExecuteCommandLists`; one `WaitFrameEventX`. It then crashed. `GameInputCreate` failed (Wine stub), so #31 was added.
4. Same as 3, with GameInput now OK (the virtual pad registers). The **same crash**: `gbxgame+0x17646e0 mov rax,[rcx+8]`, `rcx=0`. No `PresentX` yet. The user saw a black, then white, window: Wine's empty window, not title frames.

Disassembly of the crash, done on the Deck with objdump. `.text` is at VA `0x140001000`, file offset `0x400`.
- Frame 2, `0x140a06e40..`: a per-frame loop over sub-systems at `[this+0x390/0x398/0x3a0/0x3a8/0x3b0/0x3e0]`, each null-checked. The crash path is `[this+0x3a8]` → `0x140cf0920`.
- Frame 1, `0x140cf0f53..0x140cf0f85`:
  - `rsi = getter([[[rdi+8]][0]+0x488] -> 0x1416c6f00 ...)`;
  - the getter at `0x1416d28e0` returns `[[rcx+0x18]+0x88]`;
  - `edx` is 1 if `[[rdi+8]+0x68] != 0` or `[[rdi+8]+0x14] == 2` (looks like app-state / constrained / unfocused);
  - then it calls the thunk `0x1416fc0d0`: `rcx = [[rsi+8]+0x30]`; `jmp 0x1417646e0` (setter `[[rcx+8]+0x1e30] = edx`).
- **`[[rsi+8]+0x30]` is null**: a sub-object that was never created. The crash comes right after the title's second `RegisterAppStateChangeNotification` / `RegisterAppConstrainedChangeNotification` pair (context 0).

**Hypothesis (unconfirmed):** this is the audio sub-system, and the flag means "mute while unfocused". The object is null because XAudio2 creation failed quietly under Wine. Note that the runtime bundle excludes the `xaudio2_9` crate, because it does not build on a clean clone (#14-style data).

**Next step, already requested from the Deck but no result yet.** First, grep `tm_run4.txt` and the compat log for xaudio / faudio / mmdevapi / winepulse / audio. Then do one diagnostic run with `WINEDEBUG=+xaudio2,+mmdevapi,err+all,-hid,-winebth,-wineusb` (`timeout 120`). See whether `XAudio2Create` / `CreateMasteringVoice` fails and with which flags. Depending on the result:
- make the runtime's `xaudio2_9` build and stage it;
- or shim the console-only XAudio2 flags / XMA paths in front of Wine's FAudio;
- or find which other sub-system `[this+0x3a8]` is: disassemble its constructor, which writes `+0x30`.

## Other open work (`docs/ROADMAP.md` has the full plan)

- **Memory-descriptor step 3** (the runtime side, in `crates/compat/d3d12_xs`). Fill `TbUser` from the title's root arguments, map the title's GPU memory as global windows, keep the T#/S# table (`global_abi::desc_table`), and set the heap-directly-indexed root-signature flags for shaders with `Translated::mem_desc_table`. Issue #24.
- **#18 DPP on RADV:** 22 hardware cases of `dpp_lanes` fail (`row_shl:1` / `row_shr:1` with fetch_inactive=0), and every lane reads its own value. The two-shuffle theory was disproved (#29 closed). Next: take the full HLSL of case #90 and cut it down on the Deck until it passes.
- **RDNA2 left at yellow:**
  - `s_bfe` with field width ≥ 32: the rule is unconfirmed on silicon;
  - `s_sendmsghalt`: no faithful PC equivalent;
  - `ds_gws_sema_p` / `barrier` / `release_all` and `ds_ordered_count`: need wave ordering across thread groups.
- **D3D12.x:** 8 partial slots (Device 29 and 44; CommandList 64, 70, 74, 85, 86 and 101). They are GPU-verified; each still lacks a Fortnite runtime-log entry, which needs the user's PC.
- **#14 clean-clone build:** missing data blobs, Opus sources, and `d3d12_xs` shader binaries.
