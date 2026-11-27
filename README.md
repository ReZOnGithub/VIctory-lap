# VIctory-lap

A fork of the VIctory compatibility layer, which runs an Xbox Series title
natively on Windows from files the user supplies, tuned for **NVIDIA cards
and Windows 10**.

**This repository contains no game files, code or assets.** You need your own
copy of the game. It is not affiliated with or endorsed by the game's
developer or publisher.

## Credits

Nearly everything here is other people's work:

- **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**:
  the tree this fork is built on. The "renderer ports" (the title's descriptor
  stores reporting their slots, the guards' and journal's lock work), the
  lighting and texture work that gives the picture below, the frame timeline
  and the launcher are Belobug's.
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)** and its
  contributors: the frame-rate work under that (flights, the relay, card
  copies, async submission).
- **[Marvisak/VIctory](https://github.com/Marvisak/VIctory)** and its
  contributors: the layer itself, the shader translator, the host copies.
- Ideas from [stellarr-studio/kelvaroq](https://github.com/stellarr-studio/kelvaroq)
  and [xcl-dev/xcl](https://github.com/xcl-dev/xcl).

The earlier VIctory-lap tree (built on victory-fork) is on the branch
`victory-lap-v1`.

## Where it stands (2026-11-27)

Tested on one machine: Windows 10, Ryzen 7 3700X, 32 GB RAM, **GeForce RTX
5070**, ray tracing off, Series S profile (1080p).

| | |
|---|---|
| How far it plays | Boot, menus, the prison opening and its cutscenes, on to **the first boat mission** (driving the boat in open water), where testing is now |
| Frame rate at the boat mission | **21 to 22 fps** (21.3 to 22.0 at 3,450 draws a frame) |
| Frame rate in the prison opening | about 9 fps (4,500 draws a frame) |
| Menus and videos | 30 to 60 fps |
| Memory | 24 to 27 GB of private memory: a 32 GB PC pages now and then; 64 GB is comfortable |

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
| The same from 64 KB | | **21.3 to 22.0** |

- **Windows 10 boot fix:** the Agility SDK export (`apps/victory/build.rs`,
  `src/agility.rs`). Without it Windows 10's D3D12 refuses the boot screen's
  root signatures and the title crashes before it draws.
- **The fastest settings are the defaults** (`nv_defaults` in
  `apps/victory/src/run.rs`; `VICTORY_NV_DEFAULTS=0` turns them off). Every
  `VICTORY_...` switch is described in a comment where it is read.
- **A shortcut to know of:** `VICTORY_LAND_NOT_KB=64` (a default here) leaves
  what shaders write into buffers of 64 KB and more on the card. Nothing in
  the scenes played reads those on the CPU, and the picture is the same
  (small ones are read: with none brought down the picture goes white).
  If a later scene looks wrong, run with `VICTORY_LAND_NOT_KB=100000000`
  and tell us.
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

Before the first run, ahead of time (both can be stopped and go on later):

```powershell
target\release\victory.exe shaders <the shader archives>   # every shader translated, minutes
target\release\victory.exe warm --used                     # pipelines earlier runs drew with, for a new driver or build
```

Commands, debugging switches and upstream's own description:
[docs/README-fxceless.md](docs/README-fxceless.md). Every `VICTORY_...` switch is
described in a comment where it is read.

## License

GPL-3.0-or-later, as VIctory and the forks this is built on.
