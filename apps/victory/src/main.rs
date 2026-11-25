//! `victory`: the command-line front end.
//!
//! Every command reads the user's own files and writes only to stdout or to
//! the user's cache directory (outside the repository).

#[cfg(windows)]
mod agility;
mod census;
mod inspect;
mod run;
mod shader;
mod translate;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "victory",
    version,
    about = "Run an Xbox Series title natively on Windows"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarise one PE image: headers, sections, imports, TLS, build id.
    Inspect {
        file: PathBuf,
        /// List every imported function.
        #[arg(long)]
        imports: bool,
        /// List every exported function.
        #[arg(long)]
        exports: bool,
    },
    /// Aggregate the imports of every PE image under the given paths and
    /// show which ones our runtime covers. Defaults to the configured
    /// game and test-title directories.
    Census {
        paths: Vec<PathBuf>,
        /// Do not count host pass-through as covered.
        #[arg(long)]
        strict: bool,
        /// List every missing `dll!function`, most used first.
        #[arg(long)]
        missing: bool,
    },
    /// List the `dll!export` pairs an import library describes.
    LibExports {
        lib: PathBuf,
        /// List every export, not just per-DLL counts.
        #[arg(long)]
        list: bool,
    },
    /// Find shader containers in files or directories (also inside engine
    /// archives, if uncompressed) and report their layout.
    ShaderInfo {
        paths: Vec<PathBuf>,
        /// Print one line per container found.
        #[arg(long)]
        verbose: bool,
        /// Also list the decoded native instructions (implies --verbose).
        #[arg(long)]
        disasm: bool,
    },
    /// Run every shader found (containers, SGD5 files) through the
    /// translator and report what it cannot handle yet.
    Translate {
        paths: Vec<PathBuf>,
        /// Check emitted DXIL with dxil.dll's validator (`VICTORY_DXIL`,
        /// else the newest Windows SDK's).
        #[arg(long)]
        validate: bool,
        /// Also create a host D3D12 pipeline for each emitted shader.
        #[arg(long)]
        pipelines: bool,
        /// Print one line per program and outcome.
        #[arg(long)]
        list: bool,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Warm the run's shader cache: a file of shader hashes (the 16
        /// hex digits a run's log names a shader by, anywhere in a line);
        /// the containers with those hashes are translated into the
        /// cache and nothing else is done. Set `VICTORY_HOST_WAVE` and
        /// `VICTORY_VIEW_LIMIT` to what the run's log says of the host.
        #[arg(long)]
        warm: Option<PathBuf>,
    },
    /// Prepare, map and bind a title executable; with --execute, run it.
    Run {
        exe: PathBuf,
        /// What unimplemented imports do when called.
        #[arg(long, value_enum, default_value_t = Unimplemented::Trap)]
        unimplemented: Unimplemented,
        /// Disable host pass-through (everything must be implemented by us).
        #[arg(long)]
        strict: bool,
        /// Actually start the title. Without it, stop after binding.
        #[arg(long)]
        execute: bool,
        /// The console model the title sees (device type, memory).
        #[arg(long, value_enum, default_value_t = Console::SeriesX)]
        console: Console,
        /// Arguments for the title, after `--` (it reads the whole command
        /// line; a title that relaunches itself passes some).
        #[arg(last = true)]
        title_args: Vec<String>,
    },
    /// Show the resolved configuration and check it.
    Config,
}

#[derive(Clone, Copy, ValueEnum)]
enum Unimplemented {
    Trap,
    Zero,
}

#[derive(Clone, Copy, ValueEnum)]
enum Console {
    SeriesX,
    SeriesS,
}

/// The frame-rate settings measured together in fork PR #995 (its
/// `docs/fps-notes.md`, 2.5 -> 7.5 frames a second on an RTX 5070 Ti), set
/// for every switch the environment leaves unset. `VICTORY_FAST_DEFAULTS=0`:
/// none of them. Left out of that list: `VICTORY_WAVE_WIDE=0` (it drops the
/// wave-wide visibility and tiling passes: walls missing on 32-lane cards)
/// and `VICTORY_DISPLAY_MODE` (picked from the host display already).
const FAST_DEFAULTS: &[(&str, &str)] = &[
    ("VICTORY_CHEAP_LOOKS", "1"),
    // A draw whose pipeline is still being made is left out for a frame or
    // two instead of the title waiting up to 500 ms a frame for it (runs
    // g1, g2: 35-39 s of 240 waited, one 270 ms title stall a frame).
    ("VICTORY_PIPELINE_WAIT_MS", "30"),
    // The heap sync's pages and its records left to draws on four threads:
    // with a lock per group of slots (`descriptor::host::Made`) the jobs no
    // longer take turns at one lock.
    ("VICTORY_SYNC_WIDE", "8"),
    ("VICTORY_CARD_BUFFERS_MIN_KB", "64"),
    ("VICTORY_ASYNC_TEXTURES", "4"),
    ("VICTORY_ASYNC_TEXTURES_MB", "256"),
    ("VICTORY_STAGING_POOL_MB", "128"),
    ("VICTORY_HEAP_TEXTURE_MS", "60"),
    ("VICTORY_HEAP_ROLLING_RESCAN", "1"),
    ("VICTORY_LATE_WAIT", "3"),
    ("VICTORY_WIDE_BACK", "1"),
    ("VICTORY_JOIN_FETCH", "1"),
    ("VICTORY_CARD_BUFFERS_MIN_MB", "1"),
    ("VICTORY_CARD_BUFFERS_MB", "1024"),
    ("VICTORY_COMMIT_FLOOR_MB", "6144"),
    ("VICTORY_DECLARE_ONCE", "0"),
    ("VICTORY_FINE_PAGES", "1"),
    ("VICTORY_HEAP_SLOT_CACHE", "1"),
    ("VICTORY_ASK_ONCE_VIEWS", "1"),
    ("VICTORY_WIDE_COPY", "1"),
    ("VICTORY_WIDE_MERGE", "1"),
    ("VICTORY_WIDE_FILLS", "1"),
    ("VICTORY_CARD_NO_PC_COPY", "0"),
    ("VICTORY_HEAP_RECHECK", "0"),
    ("VICTORY_REUPLOAD_WHEN_FULL", "0"),
    ("VICTORY_VIEW_GUARD", "0"),
    ("VICTORY_HEAP_SYNC_TABLES", "2"),
    ("VICTORY_TABLE_INDEXED", "1"),
    ("VICTORY_GPU_UPLOAD_COPIES", "1"),
    ("VICTORY_GPU_UPLOAD_COPIES_MB", "4096"),
    ("VICTORY_TABLE_ON_CARD", "1"),
    ("VICTORY_INDIRECT_BATCH", "3"),
    // (windows and batched clamps do not go together: PR #995)
    ("VICTORY_INDIRECT_WINDOW", "0"),
    ("VICTORY_GUARD_PLACEHOLDERS", "1"),
    ("VICTORY_AHEAD_INDEX", "1"),
    ("VICTORY_VIEWS_TRIM", "1"),
    ("VICTORY_VIEW_KNOWN", "1"),
    ("VICTORY_DECLARED_WIDE", "1"),
    ("VICTORY_WIDE_TEXTURES", "1"),
    ("VICTORY_STALE_ONCE", "1"),
    ("VICTORY_CARD_FETCH_INFLIGHT", "2"),
    ("VICTORY_SYNC_NULL_SKIP", "1"),
    // Copies of title memory made in place (fork 0b0074e: the submit
    // thread's kernel time 44 -> 20 ms a frame, the picture the same).
    ("VICTORY_COPY_PLAIN", "1"),
];

fn fast_defaults() {
    if std::env::var_os("VICTORY_FAST_DEFAULTS").is_some_and(|v| v == "0") {
        return;
    }
    for (name, value) in FAST_DEFAULTS {
        if std::env::var_os(name).is_none() {
            // SAFETY: first thing in `main`, before any other thread starts.
            unsafe { std::env::set_var(name, value) };
        }
    }
}

fn main() -> Result<()> {
    fast_defaults();
    victory_core::logging::init("info");
    // The audio recorder `run` starts for `VICTORY_AUDIO_CAPTURE`.
    #[cfg(all(windows, feature = "diagnostics"))]
    if victory_hle::modules::audio::capture::child() {
        return Ok(());
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect {
            file,
            imports,
            exports,
        } => inspect::run(&file, imports, exports),
        Command::Census {
            paths,
            strict,
            missing,
        } => census::run(paths, strict, missing),
        Command::LibExports { lib, list } => inspect::lib_exports(&lib, list),
        Command::ShaderInfo {
            paths,
            verbose,
            disasm,
        } => shader::run(paths, verbose || disasm, disasm),
        Command::Translate {
            paths,
            validate,
            pipelines,
            list,
            jobs,
            warm,
        } => {
            // `VICTORY_VIEW_LIMIT=<null slot>`: translate as on a host whose view heap is capped
            if let Some(n) = std::env::var("VICTORY_VIEW_LIMIT").ok().and_then(|v| v.parse().ok()) {
                victory_shader::emit::set_view_heap_limit(n);
            }
            // `VICTORY_HOST_WAVE=32`: translate as for a GPU whose waves are at most 32 lanes
            if let Some(n) = std::env::var("VICTORY_HOST_WAVE").ok().and_then(|v| v.parse().ok()) {
                victory_shader::emit::set_host_max_wave(n);
            }
            if let Some(file) = warm {
                return translate::warm(paths, &file, jobs);
            }
            translate::run(paths, validate, pipelines, list, jobs)
        }
        Command::Run {
            exe,
            unimplemented,
            strict,
            execute,
            console,
            title_args: _,
        } => run::run(&exe, unimplemented, strict, execute, console),
        Command::Config => show_config(),
    }
}

fn show_config() -> Result<()> {
    let cfg = victory_core::Config::load()?;
    let status = |p: &Option<PathBuf>| match p {
        Some(p) if p.exists() => format!("{}", p.display()),
        Some(p) => format!("{}  (does not exist)", p.display()),
        None => "(not set)".to_owned(),
    };
    println!(
        "config file : {}",
        cfg.source
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!("game_dir    : {}", status(&cfg.game_dir));
    println!("cache_dir   : {}", cfg.cache_dir.display());
    println!("data_dir    : {}", cfg.data_dir.display());
    for t in &cfg.test_titles {
        println!("test_title  : {}", status(&Some(t.clone())));
    }
    for p in cfg
        .game_dir
        .iter()
        .chain(&cfg.test_titles)
        .chain([&cfg.cache_dir, &cfg.data_dir])
    {
        if let Err(e) = victory_core::ensure_outside_repo(p) {
            println!("POLICY VIOLATION: {e}");
        }
    }
    Ok(())
}

/// Paths given on the command line, or the configured game + test titles.
pub(crate) fn default_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    if !paths.is_empty() {
        return Ok(paths);
    }
    let cfg = victory_core::Config::load()?;
    let all: Vec<PathBuf> = cfg.game_dir.into_iter().chain(cfg.test_titles).collect();
    anyhow::ensure!(
        !all.is_empty(),
        "no paths given and none configured (see victory.example.toml)"
    );
    Ok(all)
}
