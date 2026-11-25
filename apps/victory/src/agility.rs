//! Selects the D3D12 Agility SDK runtime for this process.
//!
//! The host's root signatures use the shader model 6.6 flags
//! `CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED` and `SAMPLER_HEAP_DIRECTLY_INDEXED`.
//! The runtime that ships with Windows 10 does not know them ("Unsupported
//! bit-flag set"), so every host pipeline is refused. `d3d12.dll` looks for
//! these two exports in the process's executable and, when it finds them,
//! loads `D3D12Core.dll` from the named folder next to it instead.
//!
//! The version must match the `D3D12Core.dll` in that folder (the NuGet
//! package `Microsoft.Direct3D.D3D12`); see `docs/agility-sdk.md`. When the
//! folder or the DLL is missing, Windows falls back to its own runtime.
//! `build.rs` exports the symbols.

use std::ffi::c_char;

/// The Agility SDK's runtime version (the package's `1.616.1`).
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static D3D12SDKVersion: u32 = 616;

/// A pointer in a static, which Rust wants to be `Sync`.
#[repr(transparent)]
pub struct SdkPath(*const c_char);

// SAFETY: it points at a string literal that is never written.
unsafe impl Sync for SdkPath {}

/// The folder of `D3D12Core.dll`, relative to the executable.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static D3D12SDKPath: SdkPath = SdkPath(c".\\D3D12\\".as_ptr());
