//! Fingerprints this crate's sources (and the workspace's `Cargo.lock`) so
//! that the translated-shader cache can be keyed by what translates, not by
//! the whole executable: a change elsewhere (the GPU crate, the front end)
//! then no longer throws away every cached translation.

use std::fs;
use std::path::{Path, PathBuf};

/// FNV-1a over a byte string, continuing from `h`.
fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    // The workspace's lock file: a dependency's version can change a translation.
    files.push(root.join("..").join("..").join("Cargo.lock"));
    files.push(root.join("Cargo.toml"));
    files.sort();
    // Two passes with different bases make a 128-bit fingerprint.
    let (mut a, mut b) = (0xcbf2_9ce4_8422_2325u64, 0x8422_2325_cbf2_9ce4u64);
    for file in &files {
        let Ok(bytes) = fs::read(file) else { continue };
        let name = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");
        for h in [&mut a, &mut b] {
            *h = fnv(*h, name.as_bytes());
            *h = fnv(*h, &(bytes.len() as u64).to_le_bytes());
            *h = fnv(*h, &bytes);
        }
    }
    println!("cargo:rustc-env=VICTORY_SHADER_SOURCE_HASH={a:016x}{b:016x}");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
}
