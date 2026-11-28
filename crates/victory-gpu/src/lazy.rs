//! What shaders wrote goes into title memory when the title first touches it
//! (`VICTORY_LAND_LAZY=1`; off by default: everything lands after each
//! flight, as before; `=check` lands as before and keeps the books of this
//! beside it, to validate them).
//!
//! Why: at the boat the frame is paced by the title waiting for what the card
//! wrote, which it is told of only once a landing is done (`Host::relay_land`
//! tells the bottom-of-pipe writes and fence signals behind a flight after the
//! fetch from the card and the merge). Run s2 (default) 24.1 fps, 473 ms of
//! the process a frame; l1 (`VICTORY_LAND_NOT_KB=4`, almost nothing landed)
//! 24.1 and 445 ms, the submitting thread then idle 10 ms a frame; l2 (the
//! same with `VICTORY_TELL_AT_SEND=1`, told at send: wrong for read-backs)
//! 26.3 fps, the card 81 % busy.
//!
//! How. When the card has run a flight, the pieces its shaders wrote in a
//! copy in the card's memory are not fetched: each is noted here ("the
//! newest bytes are on the card"), per 64 KiB block of title memory, and the
//! 4 KiB pages of title memory it touches are made no-access. The title is
//! then told at once. A title thread that reads or writes such a page faults
//! (`crate::watch`'s handler); it asks the relay's thread (the only one that
//! lands, so landings keep their order and the numbering of sends they rely
//! on) to land the run of blocks around it, up to [`run_bytes`], and waits.
//! The relay fetches them on its copy queue, opens the pages and merges them
//! exactly as the eager landing would (`Shadow::land_card`: what the card
//! wrote where it differs from the CPU's side, title bytes not yet on the
//! card left out by the same notes of sends, `card.dirty`/`card.sent`), and
//! the faulting instruction runs again. A later flight that writes the block
//! arms it again; pages already no-access cost no protection call.
//!
//! What may be armed ([`Why`] counts the rest, which lands as before): only
//! copies in the card's memory, not windows, of private memory the kernel
//! write-watches (`Shadow::plainly_watched`, one `VirtualQuery` per copy
//! for "private, committed, read-write"), with no view of physical pages and
//! no guard of ours over it, and no other copy over its pages. View memory
//! (the title's physical pages, its big GPU heap) stays eager: the guards'
//! protection, the views' aliases and the runtime's remapping all change its
//! pages, and a trap there would be lost or fight the guards.
//!
//! Lock order: a thread that waits for a landing holds no lock of ours
//! (title code, or our code that asked before taking any). The relay takes
//! `Shadow::merging` (shared) then [`REG`]; never [`REG`] across a wait for a
//! GPU fetch; touching threads take only [`REG`] and wait on [`LANDED`]. The
//! relay never waits for a touching thread. Our helpers that work while the
//! relay or the submitting thread waits for them (the landing and under-writes
//! pools) never wait: a trap they meet is given up (opened without landing,
//! counted) rather than deadlock. Our copies (`crate::safecopy`) that fault on
//! a trap wait at most [`OURS_MOST`] and then fail as for memory that is not
//! there, again counted.
//!
//! The one gap: private memory has no second view, so the relay opens a
//! run's pages and then merges into them; another title thread that reads
//! those pages in those microseconds, without having faulted first, can see
//! part of the merge. A thread that faulted waits until it is whole.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, OnceLock};

use crate::shadow::Shadow;

/// The unit entries are kept in (of title memory); a card piece lies in one.
pub(crate) const BLOCK: usize = 64 << 10;
/// The unit pages are trapped in.
const PAGE: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Off,
    Lazy,
    Check,
}

/// See the file. Needs the relay's thread with its copy queue, the landing
/// threads and our own copies (`VICTORY_RELAY`, `VICTORY_RELAY_COPY`,
/// `VICTORY_LAND`, `VICTORY_SAFE_COPY`), and no `VICTORY_LAND_EVERY_MS`.
pub(crate) fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let asked = std::env::var_os("VICTORY_LAND_LAZY");
        let mode = if asked.as_ref().is_some_and(|v| v == "1") {
            Mode::Lazy
        } else if asked.is_some_and(|v| v == "check") {
            Mode::Check
        } else {
            Mode::Off
        };
        if mode == Mode::Off {
            return Mode::Off;
        }
        let can = crate::safecopy::on() && crate::land::on() && crate::shadow::relay() && crate::host::relay_copy_on() && !crate::host::land_every_on();
        if !can {
            tracing::warn!(
                "VICTORY_LAND_LAZY needs VICTORY_RELAY, VICTORY_RELAY_COPY=1, VICTORY_LAND, VICTORY_SAFE_COPY and no VICTORY_LAND_EVERY_MS: off"
            );
            return Mode::Off;
        }
        tracing::info!(?mode, run_kb = run_bytes() >> 10, least_kb = least() >> 10, hot = hot(), "what shaders wrote lands when the title first touches it (VICTORY_LAND_LAZY)");
        mode
    })
}

pub(crate) fn on() -> bool {
    mode() != Mode::Off
}

/// How much is landed around a touch at most (`VICTORY_LAND_LAZY_RUN_KB`,
/// 1024): the blocks of the same copy that follow one another, armed. A
/// fetch costs a trip to the card whatever its size, and a title that reads a
/// buffer reads on.
fn run_bytes() -> usize {
    static RUN: OnceLock<usize> = OnceLock::new();
    *RUN.get_or_init(|| {
        std::env::var("VICTORY_LAND_LAZY_RUN_KB").ok().and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1024).max(64) << 10
    })
}

/// The least copy that is armed (`VICTORY_LAND_LAZY_KB`, 0): smaller ones
/// land as before. With 1024 only what `VICTORY_LAND_NOT_KB`'s default leaves
/// out today is armed (and lands when touched, which it never did).
fn least() -> usize {
    static LEAST: OnceLock<usize> = OnceLock::new();
    *LEAST.get_or_init(|| std::env::var("VICTORY_LAND_LAZY_KB").ok().and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0) << 10)
}

/// Touches of one copy in one frame after which it lands as before for a
/// while (`VICTORY_LAND_LAZY_HOT`, 8; 0: never): a copy whose pages the title
/// shares with what shaders write faults over and over.
fn hot() -> u32 {
    static HOT: OnceLock<u32> = OnceLock::new();
    *HOT.get_or_init(|| std::env::var("VICTORY_LAND_LAZY_HOT").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(8))
}

/// Frames a hot copy lands as before.
const HOT_FRAMES: u64 = 120;

/// How long a copy of ours waits for a trap to be landed before it fails.
const OURS_MOST: std::time::Duration = std::time::Duration::from_millis(500);

/// Why a piece landed as before (counted per frame).
#[derive(Clone, Copy)]
pub(crate) enum Why {
    /// A view of physical pages, or a guard of ours, over it.
    View = 0,
    /// Not private memory the kernel watches, all of it committed read-write
    /// (or not known yet: a copy not refreshed once).
    Unwatched,
    /// Another copy over its pages.
    Shared,
    /// A window, a mapped window, in place.
    Kind,
    /// Under `VICTORY_LAND_LAZY_KB`.
    Small,
    /// Touched too often lately ([`hot`]).
    Hot,
    /// Its pages could not be made no-access.
    Protect,
    /// Its pages are landed beside a piece that lands as before.
    Beside,
}
const WHYS: usize = 8;
#[cfg(feature = "diagnostics")]
const WHY_NAMES: [&str; WHYS] = ["view memory", "not watched private", "another copy over it", "window", "small", "hot", "no protection", "beside an eager piece"];

/// Counts since the last frame line.
mod stat {
    pub(super) const ARMED_PIECES: usize = 0;
    pub(super) const ARMED_PAGES: usize = 1;
    pub(super) const PROTECTS: usize = 2;
    pub(super) const PROTECT_US: usize = 3;
    pub(super) const TOUCH_READ: usize = 4;
    pub(super) const TOUCH_WRITE: usize = 5;
    pub(super) const TOUCH_OURS: usize = 6;
    pub(super) const HANDLER_US: usize = 7;
    pub(super) const LANDINGS: usize = 8;
    pub(super) const LANDED_PIECES: usize = 9;
    pub(super) const LANDED_BYTES: usize = 10;
    pub(super) const FETCHED_BYTES: usize = 11;
    pub(super) const FOR_US: usize = 12;
    pub(super) const GAVE_UP: usize = 13;
    pub(super) const OURS_FAILED: usize = 14;
    pub(super) const LOST: usize = 15;
    pub(super) const FORGOTTEN: usize = 16;
    pub(super) const TOLD: usize = 17;
    pub(super) const CHECKED: usize = 18;
    pub(super) const DIFFER: usize = 19;
    pub(super) const DIFFER_LEFT_OUT: usize = 20;
    pub(super) const IN_FLIGHT: usize = 21;
    pub(super) const SUPERSEDED: usize = 22;
    pub(super) const SERVE_US: usize = 23;
    pub(super) const EAGER: usize = 24;
    pub(super) const COUNT: usize = EAGER + super::WHYS;
}

static STATS: [AtomicU64; stat::COUNT] = [const { AtomicU64::new(0) }; stat::COUNT];
/// The longest a title thread spent in the handler since the last line, us.
static HANDLER_MOST: AtomicU64 = AtomicU64::new(0);

fn count(which: usize, n: u64) {
    STATS[which].fetch_add(n, Ordering::Relaxed);
}

pub(crate) fn eager(why: Why, n: u64) {
    count(stat::EAGER + why as usize, n);
}

/// A bottom-of-pipe write or fence signal told when its flight's landing was
/// done (with the switch: at the card's completion, nothing fetched).
pub(crate) fn told(n: u64) {
    count(stat::TOLD, n);
}

/// Pieces armed in a 64 KiB block of title memory: one copy's.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) shadow: Arc<Shadow>,
    /// Offsets in the copy: the union of the pieces armed here.
    pub(crate) lo: usize,
    pub(crate) hi: usize,
    /// The 4 KiB pages of the block that are no-access, a bit each.
    pub(crate) pages: u16,
    /// The arming's number (a toucher waits until the entry it saw is gone).
    pub(crate) born: u64,
    /// With `=check`: the landing as before left a piece of it out
    /// (`VICTORY_LAND_NOT_KB`), so a difference is what this would add.
    pub(crate) left_out: bool,
}

#[derive(Default)]
struct Registry {
    entries: BTreeMap<usize, Entry>,
    /// Blocks touched, to be landed by the relay.
    asked: Vec<usize>,
    /// By copy number: whether its memory may be armed (`Err`: why not).
    fit: HashMap<u64, Result<(), Why>>,
    /// By copy number: the frame of its touches, how many, and the frame
    /// until which it lands as before.
    hot: HashMap<u64, (u64, u32, u64)>,
    born: u64,
}

static REG: LazyLock<Mutex<Registry>> = LazyLock::new(Mutex::default);
/// Notified when entries are landed or forgotten.
static LANDED: Condvar = Condvar::new();
/// The blocks with an entry.
static ARMED: crate::watch::Bits = crate::watch::Bits::new();
/// How many entries there are.
static ENTRIES: AtomicU64 = AtomicU64::new(0);
/// Whether a touch waits for the relay.
static ASKED: AtomicBool = AtomicBool::new(false);
/// The send numbers of flights landed so far, the highest: every send up to
/// it is on the card ([`behind`]).
static BEHIND: AtomicU64 = AtomicU64::new(0);

fn reg() -> MutexGuard<'static, Registry> {
    REG.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    /// The relay's thread.
    static RELAY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The relay is inside a lazy landing.
    static SERVING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A helper that someone who lands may be waiting for: it never waits.
    static NO_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn this_is_the_relay() {
    RELAY.set(true);
}

pub(crate) fn this_never_waits() {
    NO_WAIT.set(true);
}

pub(crate) fn serving(on: bool) -> bool {
    SERVING.replace(on)
}

/// The event that wakes the relay from its wait for the card: auto-reset,
/// made once.
pub(crate) fn event() -> usize {
    static EVENT: OnceLock<usize> = OnceLock::new();
    // SAFETY: a plain event of our own, never closed.
    *EVENT.get_or_init(|| unsafe {
        windows_sys::Win32::System::Threading::CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) as usize
    })
}

fn wake() {
    // SAFETY: our own event.
    unsafe { windows_sys::Win32::System::Threading::SetEvent(event() as _) };
    crate::host::relay_wake();
}

/// Whether anything is armed.
pub(crate) fn any() -> bool {
    ENTRIES.load(Ordering::Acquire) != 0
}

/// Whether a block of `[from, to)` has an entry (title addresses).
pub(crate) fn armed_over(from: usize, to: usize) -> bool {
    any() && from < to && ARMED.any(from / BLOCK, to.div_ceil(BLOCK))
}

/// Whether a no-access page of ours lies in `[from, to)`.
pub(crate) fn trapped_over(from: usize, to: usize) -> bool {
    if !armed_over(from, to) {
        return false;
    }
    let reg = reg();
    reg.entries.range(from / BLOCK..to.div_ceil(BLOCK)).any(|(&b, e)| pages_in(b, e.pages, from, to) != 0)
}

/// Of `pages` (of block `block`), those in `[from, to)`.
fn pages_in(block: usize, pages: u16, from: usize, to: usize) -> u16 {
    let base = block * BLOCK;
    let first = from.saturating_sub(base).min(BLOCK) / PAGE;
    let last = (to.saturating_sub(base)).min(BLOCK).div_ceil(PAGE);
    if first >= last {
        return 0;
    }
    let mask = ((1u32 << last) - (1u32 << first)) as u16;
    pages & mask
}

/// The pages of block `block` that `[from, to)` touches.
fn pages_of(block: usize, from: usize, to: usize) -> u16 {
    pages_in(block, u16::MAX, from, to)
}

/// The blocks with no-access pages in `[from, to)`.
pub(crate) fn blocks_over(from: usize, to: usize) -> Vec<usize> {
    if !armed_over(from, to) {
        return Vec::new();
    }
    let reg = reg();
    reg.entries.range(from / BLOCK..to.div_ceil(BLOCK)).filter(|(b, e)| pages_in(**b, e.pages, from, to) != 0).map(|(&b, _)| b).collect()
}

/// The relay landed flights whose plans ran behind send `sent`.
pub(crate) fn flights_landed(sent: u64) {
    BEHIND.fetch_max(sent, Ordering::AcqRel);
}

/// Every send up to this is on the card (what a fetch on the relay's copy
/// queue, sent now, comes after), as the relay knows it.
pub(crate) fn behind() -> u64 {
    BEHIND.load(Ordering::Acquire).max(crate::shadow::relay_fetched_now())
}

/// A piece to arm: the copy, offsets in it, and whether the landing as
/// before left it out.
pub(crate) type Arm = (Arc<Shadow>, usize, usize, bool);

/// The relay, gathering a landing: of `pieces` (copy, offsets, place in the
/// fetch), those that may be armed. With [`Mode::Lazy`] they leave `pieces`;
/// with `=check` they stay (and land as before too). `least` is
/// `VICTORY_LAND_NOT_KB`'s: with `=check`, whether the landing as before
/// leaves the piece out.
pub(crate) fn split(pieces: &mut Vec<(Arc<Shadow>, usize, usize, u64)>, least: u64) -> Vec<Arm> {
    let check = mode() == Mode::Check;
    let frame = crate::frame::current();
    let mut arms = Vec::new();
    // What is not known of the copies is asked without the registry's lock:
    // it looks through the copies, the views and Windows, whose locks a
    // thread that waits for a landing may hold.
    let known: Vec<Option<Result<(), Why>>> = {
        let mut reg = reg();
        if reg.fit.len() > 1 << 16 {
            reg.fit.clear();
        }
        pieces.iter().map(|p| reg.fit.get(&p.0.id()).copied()).collect()
    };
    let mut asked: HashMap<u64, Result<(), Why>> = HashMap::new();
    for (piece, known) in pieces.iter().zip(&known) {
        if known.is_none() && !asked.contains_key(&piece.0.id()) {
            asked.insert(piece.0.id(), piece.0.lazy_fit(least_ok(&piece.0)));
        }
    }
    let mut reg = reg();
    let mut keep = Vec::with_capacity(pieces.len());
    for (piece, known) in pieces.drain(..).zip(known) {
        let shadow = &piece.0;
        let fit = match known {
            Some(fit) => fit,
            None => {
                let fit = asked[&shadow.id()];
                // (Not known yet: asked again.)
                if !matches!(fit, Err(Why::Unwatched)) || shadow.plainly_watched() {
                    reg.fit.insert(shadow.id(), fit);
                }
                fit
            }
        };
        let hot_now = reg.hot.get(&shadow.id()).is_some_and(|h| h.2 > frame);
        match fit {
            Ok(()) if !hot_now => {
                arms.push((shadow.clone(), piece.1, piece.2, check && shadow.span().1 >= least));
                if check {
                    keep.push(piece);
                }
            }
            Ok(()) => {
                eager(Why::Hot, 1);
                keep.push(piece);
            }
            Err(why) => {
                eager(why, 1);
                keep.push(piece);
            }
        }
    }
    *pieces = keep;
    arms
}

fn least_ok(shadow: &Shadow) -> bool {
    shadow.span().1 as usize >= least()
}

/// [`Mode::Lazy`], before the pieces that land as before (`eager`, title
/// address and bytes) are fetched: the arms whose pages one of them touches
/// land with them (taken out of `arms` and returned), and the blocks armed
/// earlier that one of them touches, or that another copy's arm is in, are
/// returned to be landed first: the landing as before writes title memory at
/// its own address.
pub(crate) fn before_eager(eager_at: &[(usize, usize)], arms: &mut Vec<Arm>) -> (Vec<Arm>, Vec<usize>) {
    let pages = |from: usize, to: usize| (from / PAGE, to.div_ceil(PAGE));
    let mut touched: Vec<(usize, usize)> = eager_at.iter().map(|&(at, n)| pages(at, at + n)).collect();
    touched.sort_unstable();
    let hits = |a: usize, b: usize| {
        let i = touched.partition_point(|t| t.1 <= a);
        touched[i..].iter().take_while(|t| t.0 < b).any(|t| t.1 > a)
    };
    let mut demoted = Vec::new();
    arms.retain(|arm| {
        let start = arm.0.span().0 as usize;
        let (a, b) = pages(start + arm.1, start + arm.2);
        let keep = !hits(a, b);
        if !keep {
            demoted.push(arm.clone());
        }
        keep
    });
    eager(Why::Beside, demoted.len() as u64);
    let mut first = Vec::new();
    if any() {
        for &(at, n) in eager_at {
            first.extend(blocks_over(at, at + n));
        }
        // (An arm that lands as before now may have an entry of its copy
        // from an earlier flight in its block.)
        for (shadow, lo, hi, _) in &demoted {
            let start = shadow.span().0 as usize;
            first.extend(blocks_over(start + lo, start + hi));
        }
        let reg = reg();
        for arm in arms.iter() {
            let block = (arm.0.span().0 as usize + arm.1) / BLOCK;
            if reg.entries.get(&block).is_some_and(|e| !Arc::ptr_eq(&e.shadow, &arm.0)) {
                first.push(block);
            }
        }
    }
    first.sort_unstable();
    first.dedup();
    (demoted, first)
}

/// The relay arms `arms`: their pages are made no-access (those already are
/// cost nothing), runs of pages that follow one another with one call. What
/// could not be armed is returned, as the whole of what its block holds
/// (offsets in the copy), to land as before: with [`Mode::Lazy`] the caller
/// lands it with the others.
pub(crate) fn arm(arms: Vec<Arm>) -> Vec<(Arc<Shadow>, usize, usize)> {
    if arms.is_empty() {
        return Vec::new();
    }
    let began = std::time::Instant::now();
    // By copy, each copy's `merging` held shared while its blocks are armed:
    // `Shadow::merge_under_writes` holds it alone while it reads title
    // memory, and looks for armed pages under it.
    let mut by: Vec<(Arc<Shadow>, Vec<(usize, usize, bool)>)> = Vec::new();
    for (shadow, lo, hi, left_out) in arms {
        match by.iter_mut().find(|b| Arc::ptr_eq(&b.0, &shadow)) {
            Some(b) => b.1.push((lo, hi, left_out)),
            None => by.push((shadow, vec![(lo, hi, left_out)])),
        }
    }
    let mut failed = Vec::new();
    for (shadow, pieces) in by {
        let _held = shadow.lazy_hold();
        let start = shadow.span().0 as usize;
        let mut reg = reg();
        reg.born += 1;
        let born = reg.born;
        // The pages newly to be no-access: (address, block).
        let mut new: Vec<(usize, usize)> = Vec::new();
        for (lo, hi, left_out) in pieces {
            let (from, to) = (start + lo, start + hi);
            let block = from / BLOCK;
            let mask = pages_of(block, from, to);
            let entry = reg.entries.entry(block).or_insert_with(|| {
                ENTRIES.fetch_add(1, Ordering::AcqRel);
                Entry { shadow: shadow.clone(), lo, hi, pages: 0, born, left_out }
            });
            // (Another copy's entry was landed first: `before_eager`.)
            if !Arc::ptr_eq(&entry.shadow, &shadow) {
                continue;
            }
            entry.lo = entry.lo.min(lo);
            entry.hi = entry.hi.max(hi);
            entry.left_out |= left_out;
            entry.born = born;
            let fresh = mask & !entry.pages;
            entry.pages |= mask;
            count(stat::ARMED_PIECES, 1);
            for p in 0..16 {
                if fresh >> p & 1 != 0 {
                    new.push((block * BLOCK + p * PAGE, block));
                }
            }
            ARMED.set(block);
        }
        new.sort_unstable();
        count(stat::ARMED_PAGES, new.len() as u64);
        let mut bad: Vec<usize> = Vec::new();
        for run in new.chunk_by(|a, b| a.0 + PAGE == b.0) {
            if !protect(run[0].0, run.len() * PAGE, true) {
                // Page by page, then: what cannot be armed lands as before.
                for &(page, block) in run {
                    if !protect(page, PAGE, true) {
                        bad.push(block);
                    }
                }
            }
        }
        bad.sort_unstable();
        bad.dedup();
        for block in bad {
            let Some(entry) = reg.entries.remove(&block) else {
                continue;
            };
            ENTRIES.fetch_sub(1, Ordering::AcqRel);
            ARMED.clear(block);
            open_pages(block, entry.pages);
            eager(Why::Protect, 1);
            failed.push((entry.shadow, entry.lo, entry.hi));
        }
    }
    count(stat::PROTECT_US, began.elapsed().as_micros() as u64);
    failed
}

/// Makes `[at, at + len)` no-access (`trap`) from read-write, or read-write
/// from no-access. Whether it was so before and is now; a page that was not
/// is left as it was.
fn protect(at: usize, len: usize, trap: bool) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_NOACCESS, PAGE_READWRITE, VirtualProtect};
    let (to, from) = if trap { (PAGE_NOACCESS, PAGE_READWRITE) } else { (PAGE_READWRITE, PAGE_NOACCESS) };
    let mut old = 0;
    count(stat::PROTECTS, 1);
    // SAFETY: pages of title memory the copy holds; only their protection
    // changes, and only between these two.
    let done = unsafe { VirtualProtect(at as *const _, len, to, &mut old) } != 0;
    if done && old != from {
        // (The first page's says for the run: put back as it was.)
        let mut again = 0;
        // SAFETY: as above.
        unsafe { VirtualProtect(at as *const _, len, old, &mut again) };
        count(stat::PROTECTS, 1);
        return false;
    }
    done
}

/// Opens `pages` of `block` (whatever is left of them armed), runs at a
/// time. Whether all were no-access.
fn open_pages(block: usize, pages: u16) -> bool {
    let mut all = true;
    let mut p = 0;
    while p < 16 {
        if pages >> p & 1 == 0 {
            p += 1;
            continue;
        }
        let first = p;
        while p < 16 && pages >> p & 1 != 0 {
            p += 1;
        }
        all &= protect(block * BLOCK + first * PAGE, (p - first) * PAGE, false);
    }
    all
}

/// An entry taken to be landed, with its block.
pub(crate) struct Taken {
    pub(crate) block: usize,
    pub(crate) entry: Entry,
}

/// The relay: the entries of `blocks` and, around each, the armed blocks of
/// the same copy that follow one another, [`run_bytes`] at most. They stay
/// armed until [`done`].
pub(crate) fn taking(blocks: &[usize]) -> Vec<Taken> {
    let reg = reg();
    let most = run_bytes() / BLOCK;
    let mut out: Vec<Taken> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &block in blocks {
        let Some(entry) = reg.entries.get(&block) else {
            continue;
        };
        if !seen.insert(block) {
            continue;
        }
        let same = |b: usize| reg.entries.get(&b).filter(|e| Arc::ptr_eq(&e.shadow, &entry.shadow));
        let mut run = vec![block];
        let (mut down, mut up) = (block, block);
        while run.len() < most {
            let mut grew = false;
            if let Some(b) = up.checked_add(1).filter(|&b| same(b).is_some()) {
                up = b;
                run.push(b);
                grew = true;
            }
            if run.len() < most
                && let Some(b) = down.checked_sub(1).filter(|&b| same(b).is_some())
            {
                down = b;
                run.push(b);
                grew = true;
            }
            if !grew {
                break;
            }
        }
        for b in run {
            if b == block || seen.insert(b) {
                out.push(Taken { block: b, entry: reg.entries[&b].clone() });
            }
        }
    }
    out.sort_by_key(|t| t.block);
    out
}

/// The relay, after the fetch of `taken`: their pages are opened, runs of
/// them at a time. Per entry, whether its pages were still no-access (a page
/// that is not was freed and given again, or its protection changed: what is
/// there now is not what the entry is of, and nothing is put there).
pub(crate) fn open(taken: &[Taken]) -> Vec<bool> {
    let began = std::time::Instant::now();
    let _reg = reg();
    // Runs over entries that follow one another.
    let mut pages: Vec<(usize, usize)> = Vec::new();
    for (i, t) in taken.iter().enumerate() {
        for p in 0..16 {
            if t.entry.pages >> p & 1 != 0 {
                pages.push((t.block * BLOCK + p * PAGE, i));
            }
        }
    }
    pages.sort_unstable();
    let mut whole = vec![true; taken.len()];
    for run in pages.chunk_by(|a, b| a.0 + PAGE == b.0) {
        if !protect(run[0].0, run.len() * PAGE, false) {
            for &(page, i) in run {
                if !protect(page, PAGE, false) {
                    whole[i] = false;
                }
            }
        }
    }
    count(stat::PROTECT_US, began.elapsed().as_micros() as u64);
    for w in &whole {
        if !w {
            count(stat::LOST, 1);
        }
    }
    whole
}

/// The relay landed `taken` (`bytes` put into title memory, `fetched` of
/// them fetched): their entries go, and whoever waits for them goes on.
pub(crate) fn done(taken: &[Taken], bytes: u64, fetched: u64, began: std::time::Instant) {
    let mut reg = reg();
    for t in taken {
        if reg.entries.get(&t.block).is_some_and(|e| e.born == t.entry.born) {
            reg.entries.remove(&t.block);
            ENTRIES.fetch_sub(1, Ordering::AcqRel);
            ARMED.clear(t.block);
        }
    }
    drop(reg);
    LANDED.notify_all();
    count(stat::LANDINGS, 1);
    count(stat::LANDED_PIECES, taken.len() as u64);
    count(stat::LANDED_BYTES, bytes);
    count(stat::FETCHED_BYTES, fetched);
    count(stat::SERVE_US, began.elapsed().as_micros() as u64);
}

/// With `=check`, of one landing's entry: bytes it would put, and of those
/// the ones title memory does not hold already.
pub(crate) fn checked(entry: &Entry, would: u64, differ: u64) {
    count(stat::CHECKED, would);
    count(if entry.left_out { stat::DIFFER_LEFT_OUT } else { stat::DIFFER }, differ);
}

/// With `=check`: a landing not compared (a flight still writes it, or the
/// landing as before writes it next).
pub(crate) fn not_compared(in_flight: bool, n: u64) {
    count(if in_flight { stat::IN_FLIGHT } else { stat::SUPERSEDED }, n);
}

/// The blocks touched, for the relay to land.
pub(crate) fn take_asked() -> Vec<usize> {
    if !ASKED.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    std::mem::take(&mut reg().asked)
}

pub(crate) fn asked() -> bool {
    ASKED.load(Ordering::Acquire)
}

/// What a touch came to.
pub(crate) enum Touch {
    /// Not ours.
    Not,
    /// Landed (or given up): the access goes again.
    Again,
    /// A copy of ours waited too long: it fails.
    Fail,
}

/// The handler: an access at `at` faulted. If it is a no-access page of
/// ours, it is landed (by the relay) and the access goes again.
pub(crate) fn touched(at: usize, write: bool, ours: bool) -> Touch {
    if !any() || !ARMED.any(at / BLOCK, at / BLOCK + 1) {
        return Touch::Not;
    }
    let began = std::time::Instant::now();
    let block = at / BLOCK;
    let bit = 1u16 << (at % BLOCK / PAGE);
    let mut reg = reg();
    let Some(entry) = reg.entries.get(&block).filter(|e| e.pages & bit != 0) else {
        drop(reg);
        // Landed since it faulted: again, if the access can be made now.
        return if reachable(at, write) { Touch::Again } else { Touch::Not };
    };
    let (born, id) = (entry.born, entry.shadow.id());
    count(
        if ours {
            stat::TOUCH_OURS
        } else if write {
            stat::TOUCH_WRITE
        } else {
            stat::TOUCH_READ
        },
        1,
    );
    if !ours && hot() > 0 {
        let frame = crate::frame::current();
        let h = reg.hot.entry(id).or_insert((frame, 0, 0));
        if h.0 != frame {
            *h = (frame, 0, h.2);
        }
        h.1 += 1;
        if h.1 >= hot() {
            h.2 = frame + HOT_FRAMES;
        }
        if reg.hot.len() > 4096 {
            reg.hot.retain(|_, h| h.2 > frame);
        }
    }
    if RELAY.get() && !SERVING.get() {
        // The relay itself (a write of its own into title memory): it lands
        // here, holding nothing a landing takes.
        drop(reg);
        if let Some(host) = crate::host::host() {
            host.lazy_land(&[block], true);
        }
        note_handler(began, ours);
        return Touch::Again;
    }
    if RELAY.get() || NO_WAIT.get() {
        // Someone who lands may be waiting for this thread: given up.
        if let Some(entry) = reg.entries.remove(&block) {
            ENTRIES.fetch_sub(1, Ordering::AcqRel);
            ARMED.clear(block);
            open_pages(block, entry.pages);
        }
        drop(reg);
        LANDED.notify_all();
        count(stat::GAVE_UP, 1);
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| {
            tracing::warn!(
                address = format_args!("{at:#x}"),
                thread = std::thread::current().name().unwrap_or("?"),
                "a trap of VICTORY_LAND_LAZY met by a thread that may not wait: opened without landing (counted as given up)"
            );
        });
        return Touch::Again;
    }
    reg.asked.push(block);
    ASKED.store(true, Ordering::Release);
    let landed = wait(reg, &[(block, born, bit)], if ours { Some(OURS_MOST) } else { None });
    note_handler(began, ours);
    if landed {
        Touch::Again
    } else {
        count(stat::OURS_FAILED, 1);
        Touch::Fail
    }
}

fn note_handler(began: std::time::Instant, ours: bool) {
    if ours {
        return;
    }
    let us = began.elapsed().as_micros() as u64;
    count(stat::HANDLER_US, us);
    HANDLER_MOST.fetch_max(us, Ordering::Relaxed);
}

/// Waits, with the registry's lock, until each of `wanted` (block, arming,
/// page bit) is no more armed as it was; at most `most`. Whether it is so.
fn wait(mut reg: MutexGuard<'static, Registry>, wanted: &[(usize, u64, u16)], most: Option<std::time::Duration>) -> bool {
    let began = std::time::Instant::now();
    let mut said = began;
    wake();
    loop {
        let left = wanted.iter().any(|&(b, born, bit)| reg.entries.get(&b).is_some_and(|e| e.born == born && e.pages & bit != 0));
        if !left {
            return true;
        }
        if most.is_some_and(|m| began.elapsed() >= m) {
            return false;
        }
        let (again, timed_out) = LANDED.wait_timeout(reg, std::time::Duration::from_millis(20)).unwrap_or_else(|e| e.into_inner());
        reg = again;
        if timed_out.timed_out() {
            // (A wake the relay did not see.)
            ASKED.store(true, Ordering::Release);
            wake();
            if said.elapsed() >= std::time::Duration::from_secs(2) {
                said = std::time::Instant::now();
                tracing::warn!(
                    waited_ms = began.elapsed().as_millis() as u64,
                    thread = std::thread::current().name().unwrap_or("?"),
                    "waiting for a landing on touch (VICTORY_LAND_LAZY)"
                );
            }
        }
    }
}

/// Whether the page of `at` can be read (`write`: written) now.
fn reachable(at: usize, write: bool) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS,
        PAGE_READWRITE, PAGE_WRITECOPY, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    if n == 0 || info.State != MEM_COMMIT || info.Protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 || info.Protect == 0 {
        return false;
    }
    !write || info.Protect & (PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY) != 0
}

/// Our own code is about to read or write title memory `[from, to)` (a
/// refresh, the kernel for the title, a write of ours): what is armed there
/// lands first, by the relay (on the relay, here). The caller holds no lock a
/// landing takes. Whether nothing was left armed there.
pub(crate) fn land_over(from: usize, to: usize) -> bool {
    let blocks = blocks_over(from, to);
    land_blocks(&blocks)
}

/// [`land_over`] of whole blocks.
pub(crate) fn land_blocks(blocks: &[usize]) -> bool {
    if blocks.is_empty() {
        return true;
    }
    if RELAY.get() {
        if SERVING.get() {
            // (The landing's own writes into the blocks it is landing.)
            return false;
        }
        count(stat::FOR_US, blocks.len() as u64);
        if let Some(host) = crate::host::host() {
            host.lazy_land(blocks, true);
        }
        return true;
    }
    if NO_WAIT.get() {
        return false;
    }
    let mut reg = reg();
    let wanted: Vec<(usize, u64, u16)> = blocks.iter().filter_map(|b| reg.entries.get(b).map(|e| (*b, e.born, e.pages))).collect();
    if wanted.is_empty() {
        return true;
    }
    count(stat::FOR_US, wanted.len() as u64);
    reg.asked.extend(wanted.iter().map(|w| w.0));
    ASKED.store(true, Ordering::Release);
    wait(reg, &wanted, None)
}

/// The kernel is about to read or write title memory `[at, at + len)` for
/// the title (a file read): it does not fault into the handler but fails.
pub(crate) fn before_kernel(at: usize, len: usize) {
    let to = at.saturating_add(len);
    if armed_over(at, to) {
        land_over(at, to);
    }
}

/// `Shadow::update` of `[from, to)` of `shadow` meets armed blocks: the
/// ranges of the copy's own entries there, to be left alone as the ranges
/// shaders write are (title memory there is older than the copy). Where the
/// journal cannot say that the refresh reads nothing of the no-access pages
/// (the title wrote a page before it was armed, or the memory is not
/// watched), what is armed there lands first, and the refresh reads it as it
/// would after a landing.
pub(crate) fn before_refresh(shadow: &Shadow, from: usize, to: usize) -> Vec<(usize, usize)> {
    for _ in 0..4 {
        let (own, runs, blocks) = {
            let reg = reg();
            let (mut own, mut runs, mut blocks) = (Vec::new(), Vec::new(), Vec::new());
            for (&b, e) in reg.entries.range(from / BLOCK..to.div_ceil(BLOCK)) {
                let pages = pages_in(b, e.pages, from, to);
                if pages == 0 {
                    continue;
                }
                blocks.push(b);
                let start = e.shadow.span().0 as usize;
                if std::ptr::eq(Arc::as_ptr(&e.shadow), shadow) {
                    let (a, z) = ((start + e.lo).max(from), (start + e.hi).min(to));
                    if a < z {
                        own.push((a, z));
                    }
                }
                for p in 0..16 {
                    if pages >> p & 1 != 0 {
                        let at = b * BLOCK + p * PAGE;
                        runs.push((at.max(from), (at + PAGE).min(to)));
                    }
                }
            }
            (own, runs, blocks)
        };
        if runs.is_empty() {
            return Vec::new();
        }
        if runs.iter().all(|&(a, z)| shadow.journal_clean(a, z)) {
            return own;
        }
        land_blocks(&blocks);
    }
    // (Armed again and again meanwhile: left alone as shaders' ranges are.)
    let reg = reg();
    reg.entries
        .range(from / BLOCK..to.div_ceil(BLOCK))
        .filter(|(_, e)| std::ptr::eq(Arc::as_ptr(&e.shadow), shadow))
        .map(|(_, e)| {
            let start = e.shadow.span().0 as usize;
            ((start + e.lo).max(from), (start + e.hi).min(to))
        })
        .filter(|r| r.0 < r.1)
        .collect()
}

/// Title memory `[at, at + len)` is freed or given again (`len` 0: the
/// allocation at `at`, whose end `end_of(inside)` says): what is armed there
/// is forgotten, not landed. Its pages go with the memory, or come back
/// read-write with a commit.
pub(crate) fn forget(at: usize, len: usize, end_of: impl FnOnce(usize) -> usize) {
    if !any() {
        return;
    }
    let mut reg = reg();
    let to = if len != 0 {
        at.saturating_add(len)
    } else {
        match reg.entries.range(at / BLOCK..).next() {
            Some((&b, _)) => end_of((b * BLOCK).max(at)),
            None => return,
        }
    };
    let blocks: Vec<usize> = reg.entries.range(at / BLOCK..to.div_ceil(BLOCK)).map(|(&b, _)| b).collect();
    let mut n = 0;
    for b in blocks {
        let Some(e) = reg.entries.get_mut(&b) else {
            continue;
        };
        let pages = pages_in(b, e.pages, at, to);
        if pages == 0 {
            continue;
        }
        e.pages &= !pages;
        n += 1;
        if e.pages == 0 {
            reg.entries.remove(&b);
            ENTRIES.fetch_sub(1, Ordering::AcqRel);
            ARMED.clear(b);
        }
    }
    drop(reg);
    if n > 0 {
        count(stat::FORGOTTEN, n);
        LANDED.notify_all();
    }
}

/// For the landing line, per frame (diagnostics builds).
#[cfg(feature = "diagnostics")]
pub(crate) fn log(frames: f64) {
    if !on() {
        return;
    }
    let s: Vec<f64> = STATS.iter().map(|s| s.swap(0, Ordering::Relaxed) as f64 / frames).collect();
    let most = HANDLER_MOST.swap(0, Ordering::Relaxed);
    let eager: Vec<String> =
        (0..WHYS).filter(|&i| s[stat::EAGER + i] > 0.0).map(|i| format!("{} {:.0}", WHY_NAMES[i], s[stat::EAGER + i])).collect();
    let kb = |v: f64| v / 1024.0;
    tracing::info!(
        "landing on touch per frame (VICTORY_LAND_LAZY={:?}): armed {:.0} pieces, {:.0} pages newly no-access, {} blocks armed now; \
         {:.0} protection calls {:.2} ms; first touches {:.0} reads, {:.0} writes, {:.0} by our copies; title threads in the handler \
         {:.2} ms (longest {:.2} ms); {:.0} landings of {:.0} blocks, {:.0} KB fetched, {:.0} KB put, {:.2} ms on the relay; {:.0} \
         blocks landed for our own readers and writers; {:.0} traps given up, {:.0} copies of ours failed, {:.0} entries whose \
         memory changed, {:.0} forgotten with their memory; told at the card's completion {:.0}; landed as before: [{}]",
        mode(),
        s[stat::ARMED_PIECES],
        s[stat::ARMED_PAGES],
        ENTRIES.load(Ordering::Relaxed),
        s[stat::PROTECTS],
        s[stat::PROTECT_US] / 1000.0,
        s[stat::TOUCH_READ],
        s[stat::TOUCH_WRITE],
        s[stat::TOUCH_OURS],
        s[stat::HANDLER_US] / 1000.0,
        most as f64 / 1000.0,
        s[stat::LANDINGS],
        s[stat::LANDED_PIECES],
        kb(s[stat::FETCHED_BYTES]),
        kb(s[stat::LANDED_BYTES]),
        s[stat::SERVE_US] / 1000.0,
        s[stat::FOR_US],
        s[stat::GAVE_UP],
        s[stat::OURS_FAILED],
        s[stat::LOST],
        s[stat::FORGOTTEN],
        s[stat::TOLD],
        eager.join(", "),
    );
    if mode() == Mode::Check {
        tracing::info!(
            "landing on touch, check: of {:.0} bytes a landing on touch would put, {:.0} differ from title memory (must be 0), \
             {:.0} more in pieces VICTORY_LAND_NOT_KB leaves out (what it would add); not compared: {:.0} landings a flight still \
             writes, {:.0} that the landing as before writes next",
            s[stat::CHECKED],
            s[stat::DIFFER],
            s[stat::DIFFER_LEFT_OUT],
            s[stat::IN_FLIGHT],
            s[stat::SUPERSEDED],
        );
    }
}
