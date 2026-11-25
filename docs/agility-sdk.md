# D3D12 Agility SDK (Windows 10)

The host's root signatures use the shader model 6.6 flags
`CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED` and `SAMPLER_HEAP_DIRECTLY_INDEXED`.
The D3D12 runtime inbox with Windows 10 rejects them
(`root signature: Unsupported bit-flag set (root signature flags c01)`), and
every host pipeline is refused. Windows 11's runtime accepts them.

`victory.exe` exports `D3D12SDKVersion` and `D3D12SDKPath`
(`apps/victory/src/agility.rs`, `apps/victory/build.rs`), so `d3d12.dll`
loads the Agility SDK's `D3D12Core.dll` from `D3D12\` next to the executable.

To install it, take `build/native/bin/x64/D3D12Core.dll` from the NuGet
package `Microsoft.Direct3D.D3D12` **1.616.1** and put it in
`target\release\D3D12\` (or next to wherever `victory.exe` runs). Keep the
package version and `D3D12SDKVersion` (616) in step. Without the DLL, Windows
falls back to its own runtime. The DLL is Microsoft's and is not committed.
