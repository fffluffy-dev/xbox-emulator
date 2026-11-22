# Roadmap

Where fucking-frankenstein goes next, in the order the work should land. Each step says what is done, what is left and what blocks it. Status: ✅ done, 🟨 in progress, ⬜ not started, ⛔ blocked outside the project.

Last update: 2026-11-22.

---

## 0. Keep the body standing

Nothing else matters if the tree does not build and the tests do not run.

| Step | State | Notes |
|---|---|---|
| CI on GitHub Actions: Linux portable tests, Windows translator tests (DXC + D3D12 WARP), workspace, d3d12_xs hardware tests, runtime bundle | 🟨 | Workflow on branch `claude/windows-ci` ([#13](https://github.com/fffluffy-dev/xbox-emulator/issues/13)). Workspace tests run in a 90-minute time box; kerneltracecontrol's ETW tests are split out because they hang on the runner. |
| Clean clone builds on Windows | 🟨 | [#14](https://github.com/fffluffy-dev/xbox-emulator/issues/14): unpublished modules, console data blobs and libopus sources. CI clones libopus at `316c675fa1c9`. |
| Portable tests build on Linux and macOS | 🟨 | [#3](https://github.com/fffluffy-dev/xbox-emulator/issues/3) / [#1](https://github.com/fffluffy-dev/xbox-emulator/pull/1). |
| A real AMD GPU in the test loop | ✅ | Steam Deck (RDNA2, Proton, vkd3d-proton, RADV). 81 of 89 GPU tests passed on the first run ([#18](https://github.com/fffluffy-dev/xbox-emulator/issues/18)). |
| More GPUs and drivers (NVIDIA, Intel, RDNA3/4) | ⬜ | Leonida runs its translator on 30+ GPU and driver pairs. We have two (WARP and the Deck). |
| Merge the open pull requests | 🟨 | #1, #2, #6, #8, #10, #12, #16, #19, #21, #23, #25. Each has its issue and tests. |

## 1. The translator (RDNA2 → HLSL / DXIL)

| Step | State | Notes |
|---|---|---|
| Every shader of the tested titles translates | ✅ | 105,506 / 105,506 in the sweep. |
| RDNA2 instructions verified on a GPU | 🟨 | 768 / 781. The rest are held at yellow in [BLOCKERS.md](BLOCKERS.md). |
| Round modes on drivers that fold `(double)(float)x` | ✅ | [#20](https://github.com/fffluffy-dev/xbox-emulator/issues/20) / [#21](https://github.com/fffluffy-dev/xbox-emulator/pull/21). The Deck shows 0 failures. |
| DPP with two shuffles on RADV | 🟨 | The full fragment fails on the Deck and smaller cuts are pushed (`claude/fp-mode-probe`). The next step is to run them and find the smallest failing case. |
| f64 denormals in `div_f64` / `trig_preop` on RADV | 🟨 | [#18](https://github.com/fffluffy-dev/xbox-emulator/issues/18). Basic f64 denormals and fma are correct on the Deck, so the cause is in these two sequences. |
| Image atomics, TFE status, store data as entry inputs | ✅ | [#6](https://github.com/fffluffy-dev/xbox-emulator/pull/6), [#2](https://github.com/fffluffy-dev/xbox-emulator/pull/2), [#12](https://github.com/fffluffy-dev/xbox-emulator/pull/12). |
| Robust against malformed input | ✅ | [#10](https://github.com/fffluffy-dev/xbox-emulator/pull/10): 300,000 fuzzed programs, 0 panics. |
| Mesh / amplification: wave size, VS inputs from FSSM | ✅ | [#23](https://github.com/fffluffy-dev/xbox-emulator/pull/23), [#16](https://github.com/fffluffy-dev/xbox-emulator/pull/16). |
| Mesh / amplification pipelines end to end in a title | ⬜ | The stages translate, but no title has drawn through them yet. |

## 2. The GTA VI resource model (memory-descriptor mode)

Two thirds of GTA VI's shaders, including all of its mesh programs, carry no root signature. They read their descriptors from memory at run time. The approach is ported from Leonida (GPL-3.0-or-later): s0–s31 come from the runtime, memory is addressed by the console GPU VA, and images are bindless. Tracking issue [#24](https://github.com/fffluffy-dev/xbox-emulator/issues/24), pull request [#25](https://github.com/fffluffy-dev/xbox-emulator/pull/25).

| Step | State | Notes |
|---|---|---|
| 1. Buffers: `s_load`, `s_buffer_load`, MUBUF through V# from memory, bounds from `num_records` | ✅ | GPU test passes on the Deck. A shader that has a root signature is unchanged. |
| 2. Images and samplers: key the T# / S# with FNV-1a, look it up in the runtime table, take the handle from `ResourceDescriptorHeap` / `SamplerDescriptorHeap` (SM 6.6) | 🟨 | Translator and host tests are done. The GPU tests (`memory_images_bind_through_the_descriptor_table`, `memory_image_misses_take_the_null_descriptor`) are waiting for CI and Deck results. |
| 3. Runtime (d3d12_xs): fill `TbUser` from the title's root arguments, map memory windows over the title's GPU heaps, keep the T# / S# table as the title writes descriptors, set the heap-indexed root-signature flags | ⬜ | Next. `Translated::mem_user_data` and `mem_desc_table` already say what each shader needs. |
| 4. More than two memory windows; MTBUF, atomics and sub-dword buffer access through memory descriptors | ⬜ | The current mode covers dword loads and stores. |
| 5. A T# that is loaded by one instruction and written by another goes through a single UAV view | ⬜ | Today loads bind an SRV and stores bind a UAV of the same T#. |
| 6. Sweep GTA VI's 26,503 shaders through the new mode | ⬜ | Needs the shader package locally. Leonida reports 26,503 / 26,503 through its own SPIR-V path. |

## 3. A second title playable

Fortnite is the only title past its menus. The aim is one more title, complete with input and sound, so the runtime is tested beyond a single engine. Details per title are in [BLOCKERS.md](BLOCKERS.md).

| Title | State | Next |
|---|---|---|
| Trackmania (Xbox One) | 🟨 | The title menu draws at 144 fps. Next: xg_x HTILE and metadata planes, stencil-only MSAA views. |
| Terraria (Xbox One) | 🟨 | Runs 60 s with no frame. Textures get GPU VA 0, so Unity waits forever on the upload. Next: the upload path for Xbox One placed textures. |
| XDefiant | ⏸ parked | Runs 140 s at 60 fps on a black frame. Aliased-VA render targets need a shared backing. |
| Minecraft | ⬜ | Imports analysed and GDK slots verified. Not brought up yet. |

## 4. Fortnite in game

| Step | State | Notes |
|---|---|---|
| Boots, login, hotfix, mode select, Battle Royale lobby | ✅ | Evidence in [MILESTONES.md](MILESTONES.md). |
| 3D scene correct (occlusion queries, materials, bloom) | 🟨 | Every sampled occlusion query reads 0. The suspects are depth bounds or the depth/stencil state ([#17](https://github.com/fffluffy-dev/xbox-emulator/issues/17) is about depth-bounds support). |
| Audio | 🟨 | The console audio path (XVMAudioService, cross-VM WASAPI) is served and tested, but titles do not produce sound yet. |
| Keyboard / controller | ⬜ | GameInput. Leonida's SDL3 GameInput shim is a candidate donor. |
| A match in a private session (offline, single player) | ⬜ | See the online policy in [BLOCKERS.md](BLOCKERS.md). |

## 5. GTA VI

| Step | State | Notes |
|---|---|---|
| Shader package examined | ✅ | 26,503 shaders, plain compressed Xbox shader containers. |
| Shaders without a root signature translate | 🟨 | Section 2. |
| Mesh pipelines built from the title's pipeline metadata | ⬜ | Leonida's pipeline metadata is the reference. |
| Assets (models, textures, audio banks) | ⬜ | Leonida exports them. Not needed to boot. |
| Boot the executable | ⛔ | No dump contains an executable. Everything above can be done without one, and checked against shaders only. |

---

## How the work is done

- One problem, one issue, one branch, one pull request. Problems found along the way get their own issue first.
- A test before each blind step: a test that runs on a GPU or the host and fails if the step is wrong (for example, the memory-descriptor GPU tests were written before the code they check).
- Only legally obtained, already-decrypted dumps of games you own. No key extraction, no decryption, and no pirated dumps.
- Donor code only under compatible licences, with credit: Leonida (GPL-3.0-or-later) yes; americas-recomp's GPL-2.0-only core no, its MIT files yes.
