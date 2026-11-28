# VIctory-lap

A fork of the VIctory compatibility layer, which runs an Xbox Series title
natively on Windows from files the user supplies, tuned for **NVIDIA cards
and Windows 10**.

It is not an emulator of the console: the title's own x86-64 code runs
directly on the PC's processor. The layer stands in for the console's
runtime (the imports a title resolves from `xgameruntime.dll`, `d3d12_xs.dll`
and the kernel side of the GDK), translates the console's shader binaries to
DXIL, and re-records the title's Direct3D 12 command lists for a PC graphics
card.

**This repository contains no game files, code or assets.** You need your own
copy of the game. It is not affiliated with or endorsed by the game's
developer or publisher.

## Credits

Built on:

- [Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD),
  the tree this fork starts from.
- [fxceless/victory-fork](https://github.com/fxceless/victory-fork) and its
  contributors.
- [Marvisak/VIctory](https://github.com/Marvisak/VIctory) and its
  contributors, the original layer.
- Ideas from [stellarr-studio/kelvaroq](https://github.com/stellarr-studio/kelvaroq)
  and [xcl-dev/xcl](https://github.com/xcl-dev/xcl).

The earlier VIctory-lap tree (built on victory-fork) is on the branch
`victory-lap-v1`.

## Status (2026-11-28)

Self-reported, measured on one machine: Windows 10, Ryzen 7 3700X (8 cores),
32 GB RAM, **NVIDIA GeForce RTX 5070**, ray tracing off, Series S profile
(1080p). The numbers are the layer's own frame meter over runs of four
minutes; nobody else has verified them.

| Milestone | State | Detail |
|---|---|---|
| Executable loads | Yes | The title's executable is loaded and patched in memory |
| Boots, first frame | Yes | Windows 10 and 11 (Windows 10 needed the Agility SDK fix below) |
| Main menu | Yes | 60 fps |
| In-game | Yes | Played from the start through the prison opening and its cutscenes into the first boat mission |
| Frame rate in-game | **24 fps** | Boat mission, open water, 3,500 draws a frame: run averages 23.8 and 24.1 fps, single readings 22.7 to 25.0 (release build, default settings, 2026-11-28). Cutscenes 24 to 28 fps |
| Frame rate in-game with DLSS frame generation (2x) | **43 to 44 fps shown** | Same scene: 21.5 to 22 frames rendered a second and as many generated (DLSS Quality, RTX 5070; Streamline reports 22 presents and 22 generated frames a second, status ok). Generated frames, not rendered ones |
| Ray tracing (the title's own, `VICTORY_BVH_BUILDER=1`) | **Runs, 19 fps** | Same scene: run average 19.4 fps rendered, single readings 18.7 to 20.0; reflections of the bridge on the water and the shadows are there. Off by default |
| 30 fps in-game, rendered | No | 24 fps today. See "Where the frame time goes" below: on this processor the title's own threads take about 220 ms of CPU a frame before the layer adds its share |
| Stable | Partly | Four-minute scripted runs repeat without a crash or a lost device; longer sessions were played by hand to the boat mission, not timed |

**What works:** boot, menus, video, save and load, cutscenes, on-foot play and
driving the boat; lighting, characters, water, HUD and minimap as described
under "How it renders".

**What is blocking 30 fps and a "stable" mark:**

- CPU: one thread prepares the title's 37 submissions a frame (about 36 ms of
  work a frame on a 3700X); the title's own threads and the layer's keep all
  eight cores busy.
- GPU: about 31 ms of work a frame, nine tenths of it the title's own lists.
- Memory: 24 to 27 GB of private memory; a 32 GB PC pages now and then.
- Picture: the faults listed under "How it renders".
- Not tested here: AMD and Intel cards, Series X profile, play beyond the
  first boat mission; ray tracing only at the boat mission.

Earlier in the same build: the prison opening ran at about 9 fps (4,500 draws
a frame) before the later changes in the table below; it has not been
measured again.

### Where the frame time goes

Measured on the machine above in the boat mission (about 3,450 draws a frame,
41 ms a frame), with the layer's own profiler and timeline. All of it is one
machine's numbers.

| Part | CPU a frame | What it is |
|---|---|---|
| The title's own threads | about 220 ms over 20 threads | The game's code, unchanged. One of them waits some 18 ms a frame for results of GPU work |
| Translating graphics calls | about 180 to 200 ms over 8 threads | 28,000 calls a frame re-recorded for the PC; three quarters of it is binding descriptors for each draw |
| Copy helpers | about 50 ms over 8 threads | Copying and comparing what the title wrote in its memory (about 40 MB a frame) |
| The thread that prepares submissions | 31 to 35 ms | Asks Windows which pages the title wrote (380 times a frame), refreshes the copies, hands 17 submissions to the card in order |
| Bringing shader results back | about 15 ms | What shaders wrote goes back into the title's memory after each piece of GPU work |
| Sound | about 10 ms | Decoding the title's audio streams |

The whole process uses 470 to 480 ms of CPU a frame, about twelve of this
processor's sixteen threads. The card is busy 70 % of the frame; nine tenths
of that is the title's own work.

Why it is this shape: the console has one memory that its processor and its
GPU share, and a title written for it writes buffers and expects the GPU to
see them, and reads back what shaders wrote, with no copy in between. A PC
with a graphics card of its own has two memories. The layer keeps a copy of
the title's buffers on the card, finds out what changed, sends that up, and
brings back what shaders wrote. Most of what is listed above is that.

### What was tried for speed, and what it gave

Kept, in the order they came (frame rate in the boat mission):

| Change | Before | After |
|---|---|---|
| Release build with thin LTO, another allocator | 12 to 14 | 16.0 to 16.5 |
| Submissions sent to the card by a thread of their own; write watch asked ahead by another | 16.5 | 17.3 to 17.5 |
| Shader results of big buffers left on the card (nothing in the scenes played reads them) | 17.5 | 20.7 |
| Textures hashed again only every 64th frame | 20.7 | about 22 |
| Fewer walks, locks and clock reads on the hot paths (several rounds) | 22 | 22.9 to 23.6 |
| Constant buffers read through slots of their own, so that their tables are read where the title has them; one asking of the write watch shared by submissions queued together | 23 | 24.1 |

Measured and not kept, because the frame rate did not move: a second stage
for the submitting thread; telling the title earlier that GPU work is done;
longer spinning in the helpers and at the title's waits; reusing whole
descriptor tables (107 of 7,071 a frame were the same as an earlier one);
profile-guided optimisation (3 %, not worth its build time); fat LTO and
newer processor instructions (slower).

One experiment is the reason for the next piece of work: with the shader
results not brought back at all and the title told at once (the picture is
wrong that way), the same scene runs at 26.3 fps. A correct form of it, the
results brought back only when the title touches them, is in the tree behind
`VICTORY_LAND_LAZY` and not yet verified.

### How it renders

- Lighting is close to retail in what has been played: lit interiors, correct
  faces and skin, sun and sky, water with reflections and the sun's glitter,
  cutscene depth of field, the HUD and the minimap.
- Characters, hair and clothes draw correctly in the prison scenes and the
  boat cutscenes.
- Known wrong: a strip of blocks along the bottom edge of the picture in some
  scenes; trees, signs and far objects can flicker; light is too bright or
  too dark for a moment after walking indoors, and rooms with little direct
  light are dark without ray tracing; textures of what just came into view
  appear late; the first minutes in a new area stutter while the driver
  builds pipelines.

## What this fork adds

Belobug's tree is tuned on an AMD card under Windows 11. On the RTX 5070
under Windows 10 it crashed at boot, then ran at 2.5 fps. Step by step:

| Change | Prison scene | Boat mission |
|---|---|---|
| As it was (after the boot fix), renderer ports 2, 6, 7 | 2.5 fps | |
| Every title buffer's copy in the card's memory | 4.0 | |
| Upload buffers pooled; draw state freed off the recording threads | 4.3 | |
| `shadow::exact`: buffers made and idle copies dropped outside its lock | 6.95 | |
| Series S, Belobug's launcher settings, ours that apply | 8.05 | |
| The title's big heap blocks write-watched (no whole 20 MB copies) | 8.66 | |
| Card copies' changes in a pending list; the line woken at landings | 8.9 (9.3 peak) | 7.6 |
| The line looks again at once while it stands | | 8.0 |
| Buffers of views' copies used again (`VICTORY_EXACT_POOL`) | | 10.5 to 12.9 |
| Frame-critical threads above normal priority | | 11.9 to 13.1 |
| The title's 30 fps pacing replaced by a 60 fps one | | 12 to 14 |
| mimalloc, write-watch askings spread over threads, smaller fixes | | 14.0 to 14.6 |
| The release build (link-time optimized) instead of the `diag` one | | 16.0 to 16.5 |
| Sends on a thread of their own; D3D12 breadcrumbs off; the write watch asked early | | 17.3 to 17.5 |
| What shaders write into buffers of 16 MB and more is no longer read back into the title's memory (`VICTORY_LAND_NOT_MB`) | | 19.1 to 19.8 |
| The same from 1 MB, and no compare pass on the card for those | | 20.4 to 21.1 |
| The same from 64 KB | | 21.0 to 21.6 |
| Guarded textures hashed again every 64th frame, not every 8th | | **21.5 to 22.3** |

- **Windows 10 boot fix:** the Agility SDK export (`apps/victory/build.rs`,
  `src/agility.rs`). Without it Windows 10's D3D12 refuses the boot screen's
  root signatures and the title crashes before it draws.
- **The fastest settings are the defaults** (`nv_defaults` in
  `apps/victory/src/run.rs`; `VICTORY_NV_DEFAULTS=0` turns them off). Every
  `VICTORY_...` switch is described in a comment where it is read.
- **A shortcut to know of:** `VICTORY_LAND_NOT_KB=1024` (a default here)
  leaves what shaders write into buffers of 1 MB and more on the card.
  Smaller ones are read by the title: with 64 KB its ray tracing fails at the
  start and fewer textures arrive while moving; with none brought down the
  picture goes white. `VICTORY_LAND_NOT_KB=64` is about 1 fps faster without
  ray tracing; `VICTORY_LAND_NOT_KB=100000000` brings everything down.
- **Not usable on NVIDIA:** renderer ports 1 and 5 (`VICTORY_PORT_CONSTANTS`,
  `VICTORY_PORT_CBV`, the title's memory read in place by the card): the card
  reads the PC's memory several times slower, and the picture is black with
  both on. They stay off.

What sets the frame now: the processor. At the boat the process keeps about
seven of eight cores busy; one thread prepares the title's submissions (about
75 ms a frame, one after the other) and eight translate its draw calls (about
350 ms a frame between them). The card itself is busy about 40 ms a frame.

## How to run it

You need Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with shader
model 6.6 and mesh shaders, Rust (the toolchain is pinned), the MSVC build
tools, and your own dump of the title.

```powershell
git clone https://github.com/ReZOnGithub/VIctory-lap
cd VIctory-lap
copy victory.example.toml victory.toml      # then set your paths, outside this folder
cargo build --locked --release -p victory --features diagnostics
target\release\victory.exe run <path of your copy's executable> --execute --console series-s
```

That line is how the numbers above were measured: the settings are built in,
and `--console series-s` is the faster 1080p profile (leave it out for Series
X). `scripts\play.ps1 -Exe <path>` is Belobug's launcher and works too.

`scripts\play.ps1 -Safe` leaves out the newest, faster ways; try it first when
something is wrong. `-RayTracing` turns the title's ray-traced indirect light on.

For PC FSR 4.1.2, set `[paths].fidelityfx_dir` in `victory.toml` or
`VICTORY_FIDELITYFX_DIR` to the folder containing AMD's signed
`amd_fidelityfx_upscaler_dx12.dll`. Use the DirectX 12 upscaler in
`Kits/FidelityFX/signedbin`. [AMD FSR SDK 2.4.0](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/tree/v2.4.0/Kits/FidelityFX/signedbin)

`scripts\play.ps1 -Exe <path> -Fsr4K quality` selects 2560×1440 input for
the retail 1.0.2.1 Series X 4K SDR profile. `balanced` selects 2258×1270
and `performance` selects 1920×1080. All three output 3840×2160. With
`victory run` directly, set `VICTORY_FSR_4K` to the preset name.
The log reports the provider at context creation and the actual input and
output sizes at the first successful temporal dispatch. The title profile
and PC FidelityFX bridge were ported from [DexNom/VIctory](https://github.com/DexNom/VIctory/commit/ab34a8b21477c94da2a6cf07249138e8bc84602e).

On a hybrid-graphics PC, the runtime and offline pipeline checks prefer a
high-performance adapter with Shader Model 6.6. The runtime logs its adapter
name, PCI vendor and device IDs, and dedicated VRAM. Set
`VICTORY_D3D12_ADAPTER` to a zero-based DXGI index or part of the adapter's
name to select it explicitly. With the launcher, pass
`-More @{VICTORY_D3D12_ADAPTER='NVIDIA'}`.

### DLSS and frame generation

Ported from [trustno1-coder/VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA)
(NVIDIA Streamline: DLSS Super Resolution, DLSS Frame Generation, and AMD FSR 3
frame generation). `Get-NVIDIA-DLSS.cmd` fetches NVIDIA's files once (about
70 MB, from NVIDIA's own GitHub, into the `nvidia-sdk` folder, which is not
committed). Then, before `victory.exe run ...`:

```powershell
$env:VICTORY_STREAMLINE_DIR = "$PWD\nvidia-sdk\Streamline\bin\x64"
$env:VICTORY_DLSS = '1'; $env:VICTORY_DLSS_MODE = 'quality'     # dlaa, quality, balanced, performance
$env:VICTORY_FRAMEGEN = '1'; $env:VICTORY_FRAMEGEN_X = '2'      # 2 to 6 frames shown for each rendered (more than 2: RTX 50)
```

- DLSS in place of the title's own upscaling is tested here (RTX 5070,
  Series S profile): one DLSS pass a frame, the picture right, the card about
  55 % busy instead of 66 %; the frame rate stays what the CPU gives.
- DLSS Frame Generation needs an RTX 40 or later **and Windows' "Hardware-
  accelerated GPU scheduling" switched on** (Settings, System, Display,
  Graphics, then restart; or `HwSchMode` = 2 under
  `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers`). With it off
  Streamline refuses frame generation and the layer says so at the start.
- Measured here at 2x (RTX 5070, boat mission): 22 frames rendered and 22
  generated a second, about 43 shown. At that base rate the generated frames
  show faults on fast camera moves.

For 3 % more frames (21.0 to 21.7 fps at the boat mission here), a
profile-guided build: `scripts\pgo.ps1 -Step train -Exe <path>` runs a
counting build for some minutes while you play, `scripts\pgo.ps1 -Step build`
builds from what it counted, and `scripts\play.ps1 -Build pgo` runs that.

Before the first run, ahead of time (both can be stopped and go on later):

```powershell
target\release\victory.exe shaders <the shader archives>   # every shader translated, minutes
target\release\victory.exe warm --used                     # pipelines earlier runs drew with, for a new driver or build
```

Commands, debugging switches and upstream's own description:
[docs/README-fxceless.md](docs/README-fxceless.md). Every `VICTORY_...` switch is
described in a comment where it is read.

## Measuring it yourself

The numbers in this file come from tools that are in the tree (diagnostics
builds):

- The frame line in the log every five seconds: frame rate, draws, megabytes
  copied, what the submitting thread did.
- `VICTORY_PROFILE=1`: a sampling profiler of the layer's and the title's
  threads, written as a report and as folded stacks for a flame graph
  (`VICTORY_PROFILE_THREADS`, `VICTORY_PROFILE_AFTER`, `VICTORY_PROFILE_FOR`,
  `VICTORY_PROFILE_OUT`).
- `VICTORY_TIMELINE=<seconds>,<frames>,<file>`: a few frames as a trace with
  every thread, wait and GPU submission, and a text summary of what each
  frame waited for.
- `VICTORY_AB=<seconds>,<switch>[,<switch>]`: switches flipped while the title
  runs, each phase's frame times logged, so that one run compares both sides
  in the same scene.
- `VICTORY_GPU_TIMING=1`: the card's time for every list, one at a time.
- `VICTORY_CAPTURE=<folder>`: frames written as pictures while it runs.

Other programs on the PC matter at this point: a browser and a chat program
open cost about 1.5 fps here, and the title in the world holds about 21 GB
of RAM on this build.

## License

GPL-3.0-or-later, as VIctory and the forks this is built on.
