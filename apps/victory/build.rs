//! Exports the Agility SDK selectors (`src/agility.rs`) from the executable,
//! where `d3d12.dll` looks for them.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        for symbol in ["D3D12SDKVersion", "D3D12SDKPath"] {
            println!("cargo:rustc-link-arg-bins=/EXPORT:{symbol},DATA");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
