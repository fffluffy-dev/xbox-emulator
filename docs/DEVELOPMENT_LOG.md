# Development log

Reverse chronological. Each entry: what was done, evidence, blockers, next action.

---

## 2026-11-22 — renamed fucking-frankenstein; GTA VI resource model ported from Leonida

- Project renamed **fucking-frankenstein** (README, host window title). The plan is rewritten in
  `docs/ROADMAP.md`.
- tombox-xshader memory-descriptor mode ([#24](https://github.com/fffluffy-dev/xbox-emulator/issues/24),
  [#25](https://github.com/fffluffy-dev/xbox-emulator/pull/25)), after Leonida's runtime contract:
  - Step 1, buffers. Shaders with no root signature take s0–s31 from cbuffer `TbUser`. They read
    `s_load` / `s_buffer_load` / MUBUF data at the console GPU address through the global windows,
    with V# bounds. The GPU test passes on the Steam Deck.
  - Step 2, images and samplers. A T# / S# is keyed with FNV-1a over its kind and first four
    dwords, looked up in a runtime table (t0) and taken from `ResourceDescriptorHeap` /
    `SamplerDescriptorHeap` (SM 6.6). A miss reads a null view and is counted. GPU tests are pushed
    (CI and Deck pending).
- Testing without the developer's PC: CI on GitHub Actions (Windows + WARP + DXC) and a Steam Deck
  as an RDNA2 test machine. Fixes from the Deck: the round-mode fold (#21) and test assumptions
  (#19). Open: DPP with two shuffles, f64 denormals in `div_f64` / `trig_preop`.
- Synced with upstream omonkeyz/xbox-emulator `main` at `1e77fcc` (its 34 merges after the
  relicense: user-data CBV and opt-in root-signature inference for shaders without RTS0,
  image atomics via `atomic_rmw`, mesh / GS / PSV fixes, console libraries 2143/2155, fast-run
  scenes, CI gate). Kept from this fork where both sides did the same thing: the MIMG TFE status
  VGPR (upstream refused TFE), store-data registers in live_in, FSSM wave size and VS inputs,
  memory-descriptor fallback (now also after upstream's "missing root signature" error).
- Next: memory-descriptor step 3 in d3d12_xs (fill `TbUser`, map windows, keep the T# / S# table).

---

## 2026-11-21 — past "Checking for updates": login complete, game-mode select

- Backend body logging showed the client requests `grant_type=client_credentials&token_type=eg1`;
  the local backend now issues `eg1~<JWT>` tokens with real `exp`/`iat`.
- Still stalled. WinHTTP tracing (ReceiveResponse / QueryHeaders / ReadData) showed zero reads:
  the server answered every request but the title never consumed a response. Cause: the
  emulator's `WinHttpOpen` hook cleared `0x30000000` (WINHTTP_FLAG_SECURE_DEFAULTS), which
  includes WINHTTP_FLAG_ASYNC (`0x10000000`), so the session ran synchronously and the
  title's completion callbacks never fired. Fix: clear only the secure-defaults bit, keep ASYNC.
- Run 15 (log evidence): 59 ReceiveResponse / 620 ReadData calls; `HandleLoginRequestComplete
  [Result=2]`; `CheckComplete UpdateSuccess`; `UI State changing from [UI.State.Startup.Login]
  to [UI.State.Startup.SubgameSelect]`. 43 distinct backend endpoints called (6 answered 404).
- Next: in-process service emulation inside the emulator (replacing the stand-in Node server)
  and getting from game-mode select into the Battle Royale lobby without mouse input.

---

## 2026-11-21 — local backend, stuck at "Checking for updates"

- Offline, login fails (`Unable to join the Fortnite login queue`). Added a local-backend mode
  to `net_guard`: `TOMBOX_NET_REDIRECT=127.0.0.1` + `TOMBOX_NET_LOCAL_PORT=3551` sends every
  WinHTTP connection to a local LawinServer, clears `WINHTTP_FLAG_SECURE` per request and the
  secure-defaults session flags (UE's session otherwise refuses plain HTTP, error 5). No traffic
  leaves the machine.
- Result: waiting room and splash screen pass; `POST /account/api/oauth/token` answered 200;
  the screen then sits at "CHECKING FOR UPDATES..." — the hotfix v2/dispatch check starts but
  no cloudstorage request follows.
- Added WinHTTP tracing (OpenRequest path, SendRequest, async REQUEST_ERROR, failed options).
  Datarouter telemetry reaches the backend (204). Xbox Live `userstats` `/batch` never arrives
  (client-cert option fails on plain HTTP) and times out after ~11 s.
- Run 8 crashed in the first callback wrapper (events routed to the wrong owner); fixed with
  per-handle parent tracking. Run 9 showed that reporting the client-cert option as successful
  makes the stats request hang instead of failing; reverted.

---

## 2026-11-21 — first boots on this machine

**Done**
- Decrypted archive.org ZIP downloaded (27,385,950,541 bytes, exact match to
  the server's content-length) and extracted in 1.3 min.
- Dump kept pristine. TomBox DLLs no longer go into the game folder: a run
  overlay at `D:\TomBox\run\Mount` mirrors the dump with junctions and
  hardlinks; only `Binaries\XSX` in the overlay gets the runtime DLLs. `G:` is
  `subst`ed to the overlay.
- Run 1: process started, GDK runtime services and the xmem allocator came up,
  then access violation at `EXE+0xa45d63`. Last compat-log lines:
  `[xcrdapi] DStorageGetFactory [real dstorage.dll not available -> E_NOTIMPL]`.
  TomBox forwards Xbox file streaming to PC DirectStorage; the machine had none,
  so the title's I/O source buffer was null (`[r12+0x18] = 0`).
- Fix: Microsoft.Direct3D.DirectStorage 1.3.0 from NuGet, x64 DLLs
  (Authenticode: Microsoft Corporation) staged in the overlay.
- Run 2: `DStorageGetFactory forwarded (PC IID) -> hr=0`. Engine init ran 11.5 s
  / 14,440 log lines, then `MFCreateDXGIDeviceManager failed: 0x80004005` and a
  fatal error. Cause: TomBox's mfplat forwarder loads the real Media Foundation
  from `mfplat_sys.dll` next to itself; it was not staged.
- Fix: system `mfplat.dll` copied into the overlay as `mfplat_sys.dll`.

- Run 3: Media Foundation forwarded. **First frames on this machine: 2,400+
  PresentX frames at 3840x2160.** Ended by mdbg's default 70 s limit, not a
  crash. Online subsystem errors (`connection_failure`, hotfix enumeration) are
  the network guard working as intended.
- The title creates its own 3840x2160 window, which spanned both monitors
  until the first present resized it. Runtime change in `d3d12_xs`
  (`present::start_window_clamp`): from device creation, a watcher fits the
  title window to the primary monitor (16:9) whenever it extends past it.
- Host window title renamed to "Xbox Emulator"; project renamed Xbox Emulator
  (repo `omonkeyz/xbox-emulator`).
- `MDBG_LIMIT=86400` in the launcher so runs last until the window is closed.

**Correction**
- An earlier note blamed a `prot=0x1->0x1` allocation. That call is
  reserve-only (`win_type=0x102000`), so NOACCESS is correct. Not a bug.

---

## 2026-11-21 — TomBox staged, Fortnite dump downloading

**Done**
- Cloned `drycheetah84/TomBox` → `D:\TomBox`.
- Baseline `cargo build --release` passed in 29 s.
- Built the six static-CRT compat DLLs (`d3d12_xs`, `xmem`, `xcrdapi`, `acphal`,
  `dxcompiler_xs`, `mfplat`) with `RUSTFLAGS=-C target-feature=+crt-static`.
  Sizes: d3d12_xs 1.85 MB, acphal 705 KB, xcrdapi 724 KB, mfplat 698 KB,
  dxcompiler_xs 689 KB, xmem 702 KB.
- Built `mdbg.exe` tracer (229 KB) from `tools/dev/mdbg.rs`.
- Wrote `D:\TomBox\run_fortnite.ps1` — extract + stage DLLs + `subst G:` + run
  under `mdbg.exe` with log summary.
- Discovered first attempt at the Fortnite dump was a 27 GB `.xvc` (encrypted
  retail container). Verified — first 64 bytes are high-entropy with no magic,
  consistent with AES ciphertext. Deleted. Downloading the archive.org
  decrypted ZIP (27.4 GB) instead. URL verified live (HTTP 200).

**Evidence**
- `.xvc` bytes 0..64 are random; a valid ZIP would start with `50 4b 03 04`.
- Microsoft Gaming Services not installed on this machine. Required for
  `xgameruntime.dll`.

**Blockers**
- Dump download in progress.
- Gaming Services install needed before first run.

**Next**
1. `run_fortnite.ps1 -Extract` once ZIP arrives.
2. `run_fortnite.ps1 -Run` — capture baseline log.
3. Decide on first contribution: keyboard input is the smallest high-value PR
   (upstream README lists input as "partial (mouse)").
