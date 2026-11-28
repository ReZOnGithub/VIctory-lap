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
//! newest bytes are on the card"), per 64 KiB block of title memory and copy,
//! and the 4 KiB pages of title memory it touches are made no-access. The
//! title is then told at once. A title thread that reads or writes such a page
//! faults (`crate::watch`'s handler); it asks the relay's thread (the only one
//! that lands, so landings keep their order and the numbering of sends they
//! rely on) to land the run of blocks around it, up to [`run_bytes`], and
//! waits. The relay fetches them on its copy queue, opens the pages and
//! merges them exactly as the eager landing would (`Shadow::land_card`: what
//! the card wrote where it differs from the CPU's side, title bytes not yet on
//! the card left out by the same notes of sends, `card.dirty`/`card.sent`),
//! and the faulting instruction runs again. A later flight that writes the
//! block arms it again; pages already no-access cost no protection call.
//!
//! Armed only where the copy holds every title byte of the page already
//! ([`arm`]): the pages are made read-only first, title memory is compared
//! with the copy's CPU side, and only then made no-access. So the title has
//! nothing on an armed page that the copy lacks, it cannot write one without
//! landing it first, and our own landings put the same bytes into both: a
//! refresh of the copy leaves its armed pages alone and never needs a landing
//! (run z4 forced 26.5 a frame there, each a trip to the card on the
//! submitting thread, because the journal took our own landings' writes for
//! the title's). A page where the title wrote what the copy has not taken
//! lands as before.
//!
//! Who else reads what is armed, and gets it landed first (and the copy then
//! lands as before for a while, [`hot`], since the reader will be back):
//! another copy over the bytes (`Shadow::update`, which also takes them
//! whatever the journal says), a texture read from them (`crate::texture`),
//! the pages kept for raw addresses (`crate::memory`): each of them trusts
//! the journal, which our landing on touch has not written yet. The kernel
//! for the title (file reads) and the relay's own writes land first too.
//!
//! What may be armed ([`Why`] counts the rest, which lands as before): copies
//! in the card's memory of private memory the kernel write-watches
//! (`Shadow::plainly_watched`, one `VirtualQuery` per copy for "private,
//! committed, read-write"), with no view of physical pages and no guard of
//! ours over it. View memory (the title's physical pages, its big GPU heap)
//! stays eager: the guards' protection, the views' aliases and the runtime's
//! remapping all change its pages, and a trap there would be lost or fight
//! the guards.
//!
//! Lock order: a thread that waits for a landing holds no lock of ours
//! (title code, or our code that asked before taking any). The relay takes
//! `Shadow::merging` (shared), then [`REG`], then [`MAP`]; never [`REG`]
//! across a wait for a GPU fetch. Readers take [`MAP`] alone, shared.
//! Touching threads take [`REG`] (and [`MAP`] shared) and wait on [`LANDED`].
//! The relay never waits for a touching thread. Our helpers that work while
//! the relay or the submitting thread waits for them (the landing and
//! under-writes pools) never wait: a trap they meet is given up (opened
//! without landing, counted) rather than deadlock. Our copies
//! (`crate::safecopy`) that fault on a trap wait at most [`OURS_MOST`] and
//! then fail as for memory that is not there, again counted.
//!
//! The one gap: private memory has no second view, so the relay opens a
//! run's pages and then merges into them; another title thread that reads
//! those pages in those microseconds, without having faulted first, can see
//! part of the merge. A thread that faulted waits until it is whole.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, OnceLock, RwLock};

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
        tracing::info!(
            ?mode,
            run_kb = run_bytes() >> 10,
            least_kb = least() >> 10,
            hot = hot(),
            shared = shared_on(),
            windows = windows_on(),
            "what shaders wrote lands when the title first touches it (VICTORY_LAND_LAZY)"
        );
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
/// shares with what shaders write faults over and over. A reader of ours
/// that needs a landing (another copy, a texture, the raw-address pages)
/// makes it hot at once.
fn hot() -> u32 {
    static HOT: OnceLock<u32> = OnceLock::new();
    *HOT.get_or_init(|| std::env::var("VICTORY_LAND_LAZY_HOT").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(8))
}

/// Frames a hot copy lands as before.
const HOT_FRAMES: u64 = 120;

/// How long a copy of ours waits for a trap to be landed before it fails.
const OURS_MOST: std::time::Duration = std::time::Duration::from_millis(500);

/// Whether copies with other copies over them are armed
/// (`VICTORY_LAND_LAZY_SHARED=0`: they land as before, as in 5a072133).
pub(crate) fn shared_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LAND_LAZY_SHARED").is_none_or(|v| v != "0"))
}

/// Whether windows (copies of title memory outside buffers) in the card's
/// memory are armed too (`VICTORY_LAND_LAZY_WINDOWS=0`: they land as before).
/// In run z4 a window's pieces over a buffer's made 82 of the buffer's pieces
/// a frame land as before ("beside an eager piece").
pub(crate) fn windows_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LAND_LAZY_WINDOWS").is_none_or(|v| v != "0"))
}

/// Why a piece landed as before (counted per frame).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Why {
    /// A view of physical pages, or a guard of ours, over it.
    View = 0,
    /// Not private memory the kernel watches, all of it committed read-write
    /// (or not known yet: a copy not refreshed once).
    Unwatched,
    /// Another copy over its pages, with `VICTORY_LAND_LAZY_SHARED=0` (by
    /// default such copies are armed: [`before_refresh`] says how the other
    /// copies get what shaders wrote).
    Shared,
    /// A mapped window, in place, not in the card's memory; a window with
    /// `VICTORY_LAND_LAZY_WINDOWS=0`.
    Kind,
    /// Under `VICTORY_LAND_LAZY_KB`.
    Small,
    /// Touched too often lately, or read by a reader of ours ([`hot`]).
    Hot,
    /// Its pages could not be made no-access.
    Protect,
    /// Its pages are landed beside a piece that lands as before, or another
    /// copy's arm has the same pages in the same landing.
    Beside,
    /// The title wrote its pages since the copy took them.
    Dirty,
}
const WHYS: usize = 9;
#[cfg(feature = "diagnostics")]
const WHY_NAMES: [&str; WHYS] = [
    "view memory",
    "not watched private",
    "another copy over it",
    "not in the card's memory, mapped or a window",
    "small",
    "hot",
    "no protection",
    "beside an eager piece or another copy's on the same pages",
    "title bytes the copy had not taken",
];

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
    /// Blocks landed for our own readers and writers, by [`super::For`].
    pub(super) const FOR: usize = 24;
    /// Pieces armed of copies with others over them: of buffers, windows,
    /// exact copies of views.
    pub(super) const SHARED: usize = 32;
    /// Landings of the relay; those that still fetched before telling (in
    /// `=check`: would in `=1`); those with copies in the PC's memory to
    /// merge.
    pub(super) const LANDINGS_ALL: usize = 35;
    pub(super) const LANDINGS_FETCH: usize = 36;
    pub(super) const LANDINGS_PC: usize = 37;
    /// `=check` at the eager merge: bytes compared, bytes differing,
    /// pieces looked at.
    pub(super) const SAMPLED: usize = 38;
    pub(super) const SAMPLE_DIFFER: usize = 39;
    pub(super) const SAMPLE_PIECES: usize = 40;
    /// Arms of two copies on the same pages in one landing (land as
    /// before); arms of two copies in one block on other pages (armed both).
    pub(super) const CONFLICT: usize = 41;
    pub(super) const BESIDE_COPY: usize = 42;
    /// Access violations the handler showed us, and those at pages armed
    /// once; `VirtualQuery` calls of ours and their us.
    pub(super) const FAULTS: usize = 43;
    pub(super) const FAULTS_OURS: usize = 44;
    pub(super) const QUERIES: usize = 45;
    pub(super) const QUERY_US: usize = 46;
    /// Pages made read-only to be compared, and compared bytes.
    pub(super) const COMPARED: usize = 47;
    pub(super) const EAGER: usize = 48;
    /// Landings that fetched for pieces of each [`super::Why`].
    pub(super) const CLASS: usize = EAGER + super::WHYS;
    pub(super) const COUNT: usize = CLASS + super::WHYS;
}

/// Who a landing for our own readers and writers was for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum For {
    /// A refresh of a copy reads a page another copy armed, of bytes outside
    /// that copy's, that the journal says the title wrote.
    Refresh = 0,
    /// A refresh of another copy over what one copy armed: it needs what
    /// shaders wrote, as it would have after an eager landing.
    OtherCopy,
    /// The kernel reads or writes for the title (file reads).
    Kernel,
    /// The relay's own writes: a value when a list has run, copies in the
    /// PC's memory, texture read-backs.
    Relay,
    /// A piece of another copy, or of the same, that lands as before.
    Eager,
    /// A texture read from armed bytes.
    Texture,
    /// The pages kept for raw addresses (`crate::memory`).
    Arena,
}
#[cfg(feature = "diagnostics")]
const FOR_NAMES: [&str; 7] = [
    "a refresh over another copy's page, the title wrote",
    "another copy's refresh",
    "the kernel",
    "the relay's own writes",
    "pieces landing as before",
    "a texture",
    "the raw-address pages",
];

static STATS: [AtomicU64; stat::COUNT] = [const { AtomicU64::new(0) }; stat::COUNT];
/// The longest a title thread spent in the handler since the last line, us.
static HANDLER_MOST: AtomicU64 = AtomicU64::new(0);

fn count(which: usize, n: u64) {
    STATS[which].fetch_add(n, Ordering::Relaxed);
}

pub(crate) fn eager(why: Why, n: u64) {
    count(stat::EAGER + why as usize, n);
}

/// Counts blocks landed for `source`.
pub(crate) fn for_us(source: For, n: u64) {
    count(stat::FOR_US, n);
    count(stat::FOR + source as usize, n);
}

/// A bottom-of-pipe write or fence signal told when its flight's landing was
/// done (with the switch: at the card's completion, nothing fetched).
pub(crate) fn told(n: u64) {
    count(stat::TOLD, n);
}

/// Whether anything was ever armed.
static EVER: AtomicBool = AtomicBool::new(false);

/// The handler: an access violation at `at` (counted once anything was ever
/// armed). Whether it is at a block armed now or once ([`touched`]).
pub(crate) fn fault_seen(at: usize) -> bool {
    if !EVER.load(Ordering::Relaxed) {
        return false;
    }
    count(stat::FAULTS, 1);
    was_armed(at)
}

/// A `VirtualQuery` of ours (`crate::watch` too), timed from `began`.
pub(crate) fn queried(began: std::time::Instant) {
    count(stat::QUERIES, 1);
    count(stat::QUERY_US, began.elapsed().as_micros() as u64);
}

/// An entry's key: the 64 KiB block of title memory, and the copy's number.
/// Two copies may have entries in one block, on other pages.
pub(crate) type Key = (usize, u64);

/// Pieces armed in a 64 KiB block of title memory, of one copy.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) shadow: Arc<Shadow>,
    /// Offsets in the copy: the union of the pieces armed here.
    pub(crate) lo: usize,
    pub(crate) hi: usize,
    /// The 4 KiB pages of the block that are no-access, a bit each; no two
    /// entries of a block share one.
    pub(crate) pages: u16,
    /// The arming's number (a toucher waits until the entry it saw is gone).
    pub(crate) born: u64,
    /// With `=check`: the landing as before left a piece of it out
    /// (`VICTORY_LAND_NOT_KB`), so a difference is what this would add.
    pub(crate) left_out: bool,
}

#[derive(Default)]
struct Registry {
    /// Entries touched, to be landed by the relay.
    asked: Vec<Key>,
    /// By copy number: whether its memory may be armed (`Err`: why not),
    /// and the kinds of other copies over it (`Shadow::lazy_fit`).
    fit: HashMap<u64, (Result<(), Why>, u8)>,
    /// The copies with others over them that were armed since the last line.
    shared: std::collections::HashSet<u64>,
    /// By copy number: the frame of its touches, how many, and the frame
    /// until which it lands as before.
    hot: HashMap<u64, (u64, u32, u64)>,
    born: u64,
}

/// What is not the entries: changed by the relay and touching threads, and
/// what [`LANDED`] waits with.
static REG: LazyLock<Mutex<Registry>> = LazyLock::new(Mutex::default);
/// The entries. Changed only by the relay, holding [`REG`]; read by anyone
/// (refreshes ask it some 20,000 times a frame, on eight threads: shared).
static MAP: RwLock<BTreeMap<Key, Entry>> = RwLock::new(BTreeMap::new());
/// Notified when entries are landed or forgotten.
static LANDED: Condvar = Condvar::new();
/// The blocks with an entry.
static ARMED: crate::watch::Bits = crate::watch::Bits::new();
/// The blocks that ever had one: a fault there may be at a page landed since.
static ONCE: crate::watch::Bits = crate::watch::Bits::new();
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

fn map() -> std::sync::RwLockReadGuard<'static, BTreeMap<Key, Entry>> {
    MAP.read().unwrap_or_else(|e| e.into_inner())
}

/// (With [`REG`] held.)
fn map_mut() -> std::sync::RwLockWriteGuard<'static, BTreeMap<Key, Entry>> {
    MAP.write().unwrap_or_else(|e| e.into_inner())
}

/// The keys of block `block`'s entries, in a range of the map.
fn block_keys(block: usize) -> std::ops::RangeInclusive<Key> {
    (block, 0)..=(block, u64::MAX)
}

/// The keys of the blocks that `[from, to)` touches.
fn range_keys(from: usize, to: usize) -> std::ops::Range<Key> {
    (from / BLOCK, 0)..(to.div_ceil(BLOCK), 0)
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

/// Whether `at` lies in a block that is armed or was: a fault there may be
/// ours (and only then is the page asked about).
pub(crate) fn was_armed(at: usize) -> bool {
    let block = at / BLOCK;
    ONCE.any(block, block + 1)
}

/// Whether a block of `[from, to)` has an entry (title addresses).
pub(crate) fn armed_over(from: usize, to: usize) -> bool {
    any() && from < to && ARMED.any(from / BLOCK, to.div_ceil(BLOCK))
}

/// Of `[from, to)`: whether a no-access page there is `shadow`'s own, and
/// whether one is another copy's. On its own the copy has nothing to take
/// (armed only where it held the title's bytes, [`arm`]).
pub(crate) fn trapped_whose(shadow: &Shadow, from: usize, to: usize) -> (bool, bool) {
    if !armed_over(from, to) {
        return (false, false);
    }
    let me = shadow.id();
    let (mut own, mut other) = (false, false);
    for (k, e) in map().range(range_keys(from, to)) {
        if pages_in(k.0, e.pages, from, to) != 0 {
            if k.1 == me {
                own = true;
            } else {
                other = true;
            }
        }
    }
    (own, other)
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

/// The entries with no-access pages in `[from, to)`.
pub(crate) fn keys_over(from: usize, to: usize) -> Vec<Key> {
    if !armed_over(from, to) {
        return Vec::new();
    }
    map().range(range_keys(from, to)).filter(|(k, e)| pages_in(k.0, e.pages, from, to) != 0).map(|(&k, _)| k).collect()
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
/// leaves the piece out. `whys` gets why the others land as before.
pub(crate) fn split(pieces: &mut Vec<(Arc<Shadow>, usize, usize, u64)>, least: u64, whys: &mut HashMap<u64, Why>) -> Vec<Arm> {
    let check = mode() == Mode::Check;
    let frame = crate::frame::current();
    let mut arms = Vec::new();
    // What is not known of the copies is asked without the registry's lock:
    // it looks through the copies, the views and Windows, whose locks a
    // thread that waits for a landing may hold.
    let known: Vec<Option<(Result<(), Why>, u8)>> = {
        let mut reg = reg();
        if reg.fit.len() > 1 << 16 {
            reg.fit.clear();
        }
        pieces.iter().map(|p| reg.fit.get(&p.0.id()).copied()).collect()
    };
    let mut asked: HashMap<u64, (Result<(), Why>, u8)> = HashMap::new();
    for (piece, known) in pieces.iter().zip(&known) {
        if known.is_none() && !asked.contains_key(&piece.0.id()) {
            let began = std::time::Instant::now();
            asked.insert(piece.0.id(), piece.0.lazy_fit(least_ok(&piece.0)));
            queried(began);
        }
    }
    let mut reg = reg();
    let mut keep = Vec::with_capacity(pieces.len());
    for (piece, known) in pieces.drain(..).zip(known) {
        let shadow = &piece.0;
        let (fit, kinds) = match known {
            Some(fit) => fit,
            None => {
                let fit = asked[&shadow.id()];
                // (Not known yet: asked again.)
                if !matches!(fit.0, Err(Why::Unwatched)) || shadow.plainly_watched() {
                    reg.fit.insert(shadow.id(), fit);
                }
                fit
            }
        };
        let hot_now = reg.hot.get(&shadow.id()).is_some_and(|h| h.2 > frame);
        match fit {
            Ok(()) if !hot_now => {
                for k in 0..3 {
                    if kinds >> k & 1 != 0 {
                        count(stat::SHARED + k, 1);
                    }
                }
                if kinds != 0 {
                    reg.shared.insert(shadow.id());
                }
                arms.push((shadow.clone(), piece.1, piece.2, check && shadow.span().1 >= least));
                if check {
                    keep.push(piece);
                }
            }
            Ok(()) => {
                eager(Why::Hot, 1);
                whys.insert(shadow.id(), Why::Hot);
                keep.push(piece);
            }
            Err(why) => {
                eager(why, 1);
                whys.insert(shadow.id(), why);
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

/// The pages of block `block` an arm touches, and its block.
fn arm_pages(arm: &Arm) -> (usize, u16) {
    let start = arm.0.span().0 as usize;
    let (from, to) = (start + arm.1, start + arm.2);
    let block = from / BLOCK;
    (block, pages_of(block, from, to))
}

/// What the relay's landing in hand fetched before it told the title, set
/// by `Host::relay_card_back` (the classes of [`Why`] a bit each, and bit 15
/// whether anything was fetched) and taken by [`landing_done`].
static LANDING_MASK: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// `Host::relay_card_back`: of the pieces fetched before the telling
/// (`pieces`, after `VICTORY_LAND_NOT_KB`), the classes (`whys`, by copy
/// number: those not there are armed, and with `=check` land as before only
/// to be compared), and `extra` (classes of arms that land as before after
/// all).
pub(crate) fn landing_fetched(pieces: &[(Arc<Shadow>, usize, usize, u64)], whys: &HashMap<u64, Why>, extra: u16) {
    let mut mask = extra;
    for piece in pieces {
        if let Some(&why) = whys.get(&piece.0.id()) {
            mask |= 1 << why as usize;
        }
    }
    if (mode() == Mode::Lazy && !pieces.is_empty()) || mask & 0x7fff != 0 {
        mask |= 1 << 15;
    }
    LANDING_MASK.fetch_or(mask, Ordering::Relaxed);
}

/// `Host::relay_land`, before it tells: one landing, with copies in the PC's
/// memory to merge or not.
pub(crate) fn landing_done(pc: bool) {
    let mask = LANDING_MASK.swap(0, Ordering::Relaxed);
    count(stat::LANDINGS_ALL, 1);
    count(stat::LANDINGS_PC, u64::from(pc));
    if mask & 1 << 15 != 0 {
        count(stat::LANDINGS_FETCH, 1);
    }
    for i in 0..WHYS {
        if mask >> i & 1 != 0 {
            count(stat::CLASS + i, 1);
        }
    }
}

/// The bits of [`landing_fetched`]'s `extra` for arms that land as before:
/// `why` of each.
pub(crate) fn why_bits(whys: &[Why]) -> u16 {
    whys.iter().fold(0, |m, &w| m | 1 << w as usize)
}

/// `=check` at the eager merge: of a piece of an armed copy, `bytes` looked
/// at, `differ` of them not as a landing on touch would have put them.
pub(crate) fn sampled(bytes: u64, differ: u64) {
    count(stat::SAMPLED, bytes);
    count(stat::SAMPLE_DIFFER, differ);
    count(stat::SAMPLE_PIECES, 1);
}

/// `=check`: whether a piece of this many bytes is compared at the eager
/// merge (about [`SAMPLE_MOST`] a frame).
pub(crate) fn sample_room(bytes: usize) -> bool {
    static FRAME: AtomicU64 = AtomicU64::new(u64::MAX);
    static TAKEN: AtomicU64 = AtomicU64::new(0);
    let frame = crate::frame::current();
    if FRAME.swap(frame, Ordering::Relaxed) != frame {
        TAKEN.store(0, Ordering::Relaxed);
    }
    TAKEN.fetch_add(bytes as u64, Ordering::Relaxed) < SAMPLE_MOST
}

/// What `=check` compares at the eager merge, a frame.
const SAMPLE_MOST: u64 = 512 << 10;

/// [`Mode::Lazy`], before the pieces that land as before (`eager`, title
/// address and bytes) are fetched. Taken out of `arms` and returned, to land
/// with them: arms whose pages one of them touches, and arms of two copies
/// on the same pages of a block (two flights' pieces must land in their
/// order, which the landing as before keeps, `crate::land::ordered`). Also
/// returned: the entries armed earlier that one of those touches, or that
/// share pages with an arm of another copy, to be landed first (the landing
/// as before writes title memory at its own address).
pub(crate) fn before_eager(eager_at: &[(usize, usize)], arms: &mut Vec<Arm>) -> (Vec<Arm>, Vec<Key>) {
    let mut demoted = Vec::new();
    {
        // By block: the pages each copy's arms touch.
        let mut by: HashMap<usize, Vec<(u64, u16)>> = HashMap::new();
        for arm in arms.iter() {
            let (block, pages) = arm_pages(arm);
            let copies = by.entry(block).or_default();
            match copies.iter_mut().find(|c| c.0 == arm.0.id()) {
                Some(c) => c.1 |= pages,
                None => copies.push((arm.0.id(), pages)),
            }
        }
        let clash = |block: usize, id: u64| {
            by.get(&block).is_some_and(|copies| {
                let mine = copies.iter().find(|c| c.0 == id).map_or(0, |c| c.1);
                copies.iter().any(|c| c.0 != id && c.1 & mine != 0)
            })
        };
        let beside = by.values().filter(|copies| copies.len() > 1).count();
        count(stat::BESIDE_COPY, beside as u64);
        arms.retain(|arm| {
            let keep = !clash(arm_pages(arm).0, arm.0.id());
            if !keep {
                count(stat::CONFLICT, 1);
                demoted.push(arm.clone());
            }
            keep
        });
    }
    let pages = |from: usize, to: usize| (from / PAGE, to.div_ceil(PAGE));
    let mut touched: Vec<(usize, usize)> = eager_at.iter().map(|&(at, n)| pages(at, at + n)).collect();
    touched.sort_unstable();
    let hits = |a: usize, b: usize| {
        let i = touched.partition_point(|t| t.1 <= a);
        touched[i..].iter().take_while(|t| t.0 < b).any(|t| t.1 > a)
    };
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
            first.extend(keys_over(at, at + n));
        }
        // (An arm that lands as before now may have an entry of its copy
        // from an earlier flight in its block.)
        for (shadow, lo, hi, _) in &demoted {
            let start = shadow.span().0 as usize;
            first.extend(keys_over(start + lo, start + hi));
        }
        let map = map();
        for arm in arms.iter() {
            let (block, pages) = arm_pages(arm);
            for (k, e) in map.range(block_keys(block)) {
                if k.1 != arm.0.id() && e.pages & pages != 0 {
                    first.push(*k);
                }
            }
        }
    }
    first.sort_unstable();
    first.dedup();
    (demoted, first)
}

/// The relay arms `arms`: their pages are made read-only, title memory there
/// is compared with the copy's CPU side (`Shadow::lazy_clean`), and they are
/// made no-access; pages already no-access cost nothing, and runs of pages
/// that follow one another take one call each time. What could not be armed
/// (no protection, or title bytes the copy has not taken) is returned with
/// why, as the whole of what its entry holds (offsets in the copy), to land
/// as before: with [`Mode::Lazy`] the caller lands it with the others.
pub(crate) fn arm(arms: Vec<Arm>) -> Vec<(Arc<Shadow>, usize, usize, Why)> {
    use windows_sys::Win32::System::Memory::{PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE};
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
        let id = shadow.id();
        let start = shadow.span().0 as usize;
        let mut reg = reg();
        reg.born += 1;
        let born = reg.born;
        // The pages newly to be no-access: (address, block).
        let mut new: Vec<(usize, usize)> = Vec::new();
        {
            let mut map = map_mut();
            for (lo, hi, left_out) in pieces {
                let (from, to) = (start + lo, start + hi);
                let block = from / BLOCK;
                let mask = pages_of(block, from, to);
                // (Another copy's entry on these pages was landed first,
                // `before_eager`; with `=check` the piece is left to the
                // landing as before.)
                let taken: u16 = map.range(block_keys(block)).filter(|(k, _)| k.1 != id).fold(0, |m, (_, e)| m | e.pages);
                if taken & mask != 0 {
                    count(stat::CONFLICT, 1);
                    continue;
                }
                let entry = map.entry((block, id)).or_insert_with(|| {
                    ENTRIES.fetch_add(1, Ordering::AcqRel);
                    Entry { shadow: shadow.clone(), lo, hi, pages: 0, born, left_out }
                });
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
                ONCE.set(block);
                EVER.store(true, Ordering::Relaxed);
            }
        }
        new.sort_unstable();
        count(stat::ARMED_PAGES, new.len() as u64);
        // First read-only: the title may still read, and a write of its own
        // faults (and waits for this lock); then compared; then no-access.
        let mut bad: Vec<(usize, Why)> = Vec::new();
        let mut readonly: Vec<(usize, usize)> = Vec::with_capacity(new.len());
        for run in new.chunk_by(|a, b| a.0 + PAGE == b.0) {
            if protect(run[0].0, run.len() * PAGE, PAGE_READONLY, PAGE_READWRITE) {
                readonly.extend_from_slice(run);
                continue;
            }
            for &(page, block) in run {
                if protect(page, PAGE, PAGE_READONLY, PAGE_READWRITE) {
                    readonly.push((page, block));
                } else {
                    bad.push((block, Why::Protect));
                }
            }
        }
        for run in readonly.chunk_by(|a, b| a.0 + PAGE == b.0) {
            count(stat::COMPARED, (run.len() * PAGE) as u64);
            if !shadow.lazy_clean(run[0].0, run[0].0 + run.len() * PAGE) {
                bad.extend(run.iter().map(|&(_, block)| (block, Why::Dirty)));
            }
        }
        bad.sort_unstable_by_key(|b| b.0);
        bad.dedup_by_key(|b| b.0);
        for run in readonly.chunk_by(|a, b| a.0 + PAGE == b.0) {
            let run: Vec<(usize, usize)> = run.iter().copied().filter(|p| bad.binary_search_by_key(&p.1, |b| b.0).is_err()).collect();
            for run in run.chunk_by(|a, b| a.0 + PAGE == b.0) {
                if protect(run[0].0, run.len() * PAGE, PAGE_NOACCESS, PAGE_READONLY) {
                    continue;
                }
                for &(page, block) in run {
                    if !protect(page, PAGE, PAGE_NOACCESS, PAGE_READONLY) {
                        bad.push((block, Why::Protect));
                    }
                }
            }
        }
        bad.sort_unstable_by_key(|b| b.0);
        bad.dedup_by_key(|b| b.0);
        if !bad.is_empty() {
            let mut map = map_mut();
            for (block, why) in bad {
                let Some(entry) = map.remove(&(block, id)) else {
                    continue;
                };
                ENTRIES.fetch_sub(1, Ordering::AcqRel);
                if map.range(block_keys(block)).next().is_none() {
                    ARMED.clear(block);
                }
                // (Read-only or no-access, whichever each reached.)
                open_any(block, entry.pages);
                eager(why, 1);
                failed.push((entry.shadow, entry.lo, entry.hi, why));
            }
        }
        drop(reg);
    }
    count(stat::PROTECT_US, began.elapsed().as_micros() as u64);
    failed
}

/// Makes `[at, at + len)` `to` from `from`. Whether it was `from` before
/// and is `to` now; a page that was not is left as it was.
fn protect(at: usize, len: usize, to: u32, from: u32) -> bool {
    use windows_sys::Win32::System::Memory::VirtualProtect;
    let mut old = 0;
    count(stat::PROTECTS, 1);
    // SAFETY: pages of title memory a copy of ours holds; only their
    // protection changes, among read-write, read-only and no-access.
    let done = unsafe { VirtualProtect(at as *const _, len, to, &mut old) } != 0;
    if done && old != from {
        // (The first page's says for the run: put back as it was.)
        let mut again = 0;
        // SAFETY: as above.
        unsafe { VirtualProtect(at as *const _, len, old, &mut again) };
        count(stat::PROTECTS, 1);
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| {
            tracing::warn!(
                address = format_args!("{at:#x}"),
                old = format_args!("{old:#x}"),
                wanted = format_args!("{from:#x}"),
                "VICTORY_LAND_LAZY: a page's protection was not what arming expected (counted as no protection)"
            );
        });
        return false;
    }
    done
}

/// Opens `pages` of `block` to read-write whatever they are now (an arming
/// that failed half way), runs at a time.
fn open_any(block: usize, pages: u16) {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
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
        let mut old = 0;
        count(stat::PROTECTS, 1);
        // SAFETY: as in `protect`.
        unsafe { VirtualProtect((block * BLOCK + first * PAGE) as *const _, (p - first) * PAGE, PAGE_READWRITE, &mut old) };
    }
}

/// An entry taken to be landed, with its key.
pub(crate) struct Taken {
    pub(crate) key: Key,
    pub(crate) entry: Entry,
}

/// The relay: the entries of `keys` and, around each, the armed blocks of
/// the same copy that follow one another, [`run_bytes`] at most. They stay
/// armed until [`done`].
pub(crate) fn taking(keys: &[Key]) -> Vec<Taken> {
    let map = map();
    let most = run_bytes() / BLOCK;
    let mut out: Vec<Taken> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &key in keys {
        if !map.contains_key(&key) || !seen.insert(key) {
            continue;
        }
        let (block, id) = key;
        let mut run = vec![key];
        let (mut down, mut up) = (block, block);
        while run.len() < most {
            let mut grew = false;
            if let Some(b) = up.checked_add(1).filter(|&b| map.contains_key(&(b, id))) {
                up = b;
                run.push((b, id));
                grew = true;
            }
            if run.len() < most
                && let Some(b) = down.checked_sub(1).filter(|&b| map.contains_key(&(b, id)))
            {
                down = b;
                run.push((b, id));
                grew = true;
            }
            if !grew {
                break;
            }
        }
        for k in run {
            if k == key || seen.insert(k) {
                out.push(Taken { key: k, entry: map[&k].clone() });
            }
        }
    }
    out.sort_by_key(|t| t.key);
    out
}

/// The relay, after the fetch of `taken`: their pages are opened, runs of
/// them at a time. Per entry, whether its pages were still no-access (a page
/// that is not was freed and given again, or its protection changed: what is
/// there now is not what the entry is of, and nothing is put there).
pub(crate) fn open(taken: &[Taken]) -> Vec<bool> {
    use windows_sys::Win32::System::Memory::{PAGE_NOACCESS, PAGE_READWRITE};
    let began = std::time::Instant::now();
    let _reg = reg();
    // Runs over entries that follow one another.
    let mut pages: Vec<(usize, usize)> = Vec::new();
    for (i, t) in taken.iter().enumerate() {
        for p in 0..16 {
            if t.entry.pages >> p & 1 != 0 {
                pages.push((t.key.0 * BLOCK + p * PAGE, i));
            }
        }
    }
    pages.sort_unstable();
    let mut whole = vec![true; taken.len()];
    for run in pages.chunk_by(|a, b| a.0 + PAGE == b.0) {
        if !protect(run[0].0, run.len() * PAGE, PAGE_READWRITE, PAGE_NOACCESS) {
            for &(page, i) in run {
                if !protect(page, PAGE, PAGE_READWRITE, PAGE_NOACCESS) {
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
    {
        let _reg = reg();
        let mut map = map_mut();
        for t in taken {
            if map.get(&t.key).is_some_and(|e| e.born == t.entry.born) {
                map.remove(&t.key);
                ENTRIES.fetch_sub(1, Ordering::AcqRel);
                if map.range(block_keys(t.key.0)).next().is_none() {
                    ARMED.clear(t.key.0);
                }
            }
        }
    }
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

/// The entries touched, for the relay to land.
pub(crate) fn take_asked() -> Vec<Key> {
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
    let block = at / BLOCK;
    if !ONCE.any(block, block + 1) {
        return Touch::Not;
    }
    count(stat::FAULTS_OURS, 1);
    let began = std::time::Instant::now();
    let bit = 1u16 << (at % BLOCK / PAGE);
    let mut reg = reg();
    let found = map().range(block_keys(block)).find(|(_, e)| e.pages & bit != 0).map(|(k, e)| (*k, e.born, e.shadow.id()));
    let Some((key, born, id)) = found else {
        drop(reg);
        // Landed since it faulted (by the relay, for another thread or a
        // reader of ours): again, if the access can be made now.
        return if reachable(at, write) { Touch::Again } else { Touch::Not };
    };
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
            host.lazy_land(&[key], true);
        }
        note_handler(began, ours);
        return Touch::Again;
    }
    if RELAY.get() || NO_WAIT.get() {
        // Someone who lands may be waiting for this thread: given up.
        let gone = {
            let mut map = map_mut();
            let gone = map.remove(&key);
            if gone.is_some() {
                ENTRIES.fetch_sub(1, Ordering::AcqRel);
                if map.range(block_keys(block)).next().is_none() {
                    ARMED.clear(block);
                }
            }
            gone
        };
        if let Some(entry) = gone {
            open_any(block, entry.pages);
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
    reg.asked.push(key);
    ASKED.store(true, Ordering::Release);
    let landed = wait(reg, &[(key, born, bit)], if ours { Some(OURS_MOST) } else { None });
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

/// Waits, with the registry's lock, until each of `wanted` (key, arming,
/// page bits) is no more armed as it was; at most `most`. Whether it is so.
fn wait(mut reg: MutexGuard<'static, Registry>, wanted: &[(Key, u64, u16)], most: Option<std::time::Duration>) -> bool {
    let began = std::time::Instant::now();
    let mut said = began;
    wake();
    loop {
        let left = {
            let map = map();
            wanted.iter().any(|&(k, born, bits)| map.get(&k).is_some_and(|e| e.born == born && e.pages & bits != 0))
        };
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
    let began = std::time::Instant::now();
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    queried(began);
    if n == 0 || info.State != MEM_COMMIT || info.Protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 || info.Protect == 0 {
        return false;
    }
    !write || info.Protect & (PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY) != 0
}

/// Our own code is about to read or write title memory `[from, to)` (the
/// kernel for the title, a write of ours): what is armed there lands first,
/// by the relay (on the relay, here). The caller holds no lock a landing
/// takes. Whether nothing was left armed there.
pub(crate) fn land_over(from: usize, to: usize, source: For) -> bool {
    let keys = keys_over(from, to);
    land_keys(&keys, source)
}

/// A reader of ours that trusts the journal (a texture, the raw-address
/// pages) is about to look at `[from, to)`: what is armed there lands first,
/// and its copies land as before for a while (the reader will be back).
/// Whether something landed: the reader takes the range whatever the journal
/// says, since its asking may have come before our write.
pub(crate) fn reader(from: usize, to: usize, source: For) -> bool {
    let keys = keys_over(from, to);
    if keys.is_empty() {
        return false;
    }
    heat(&keys);
    land_keys(&keys, source);
    true
}

/// The copies of `keys` land as before for [`HOT_FRAMES`].
fn heat(keys: &[Key]) {
    let frame = crate::frame::current();
    let mut reg = reg();
    for &(_, id) in keys {
        let h = reg.hot.entry(id).or_insert((frame, 0, 0));
        h.2 = frame + HOT_FRAMES;
    }
}

/// [`land_over`] of whole entries.
pub(crate) fn land_keys(keys: &[Key], source: For) -> bool {
    if keys.is_empty() {
        return true;
    }
    if RELAY.get() {
        if SERVING.get() {
            // (The landing's own writes into the blocks it is landing.)
            return false;
        }
        for_us(source, keys.len() as u64);
        if let Some(host) = crate::host::host() {
            host.lazy_land(keys, true);
        }
        return true;
    }
    if NO_WAIT.get() {
        return false;
    }
    let mut reg = reg();
    let wanted: Vec<(Key, u64, u16)> = {
        let map = map();
        keys.iter().filter_map(|k| map.get(k).map(|e| (*k, e.born, e.pages))).collect()
    };
    if wanted.is_empty() {
        return true;
    }
    for_us(source, wanted.len() as u64);
    reg.asked.extend(wanted.iter().map(|w| w.0));
    ASKED.store(true, Ordering::Release);
    wait(reg, &wanted, None)
}

/// The kernel is about to read or write title memory `[at, at + len)` for
/// the title (a file read): it does not fault into the handler but fails.
pub(crate) fn before_kernel(at: usize, len: usize) {
    let to = at.saturating_add(len);
    if armed_over(at, to) {
        land_over(at, to, For::Kernel);
    }
}

/// `Shadow::update` of `[from, to)` of `shadow` meets armed blocks. Returns
/// the ranges to leave alone and those to copy whatever the journal says.
///
/// The copy's own armed pages: left alone, whatever the journal says. They
/// were armed only if title memory there held nothing the copy lacked
/// ([`arm`]); since then the title cannot write them without landing them,
/// and a landing puts the same bytes into title memory and the copy. So the
/// refresh would take nothing there, and needs no landing: the journal's
/// marks on them are our own landings' writes, or from before the copy took
/// the page.
///
/// Another copy's entries over bytes of this refresh: this copy needs what
/// the shaders wrote there, as it would have read it from title memory after
/// an eager landing. That rule is exact because the landing on touch puts
/// into title memory exactly what the eager landing would (`land_card`, the
/// same pieces, the same notes of sends), and the refresh then takes it: the
/// entries land first (and their copy lands as before for a while), and
/// their bytes in this range are copied whatever the journal says, since the
/// submission's asking was made before our write. Another copy's armed page
/// where this copy has bytes outside that copy's: compared at arming with
/// that copy only, so this copy reads it if the journal says the title wrote
/// it, after landing it.
pub(crate) fn before_refresh(shadow: &Shadow, from: usize, to: usize) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let me = shadow.id();
    let mut force: Vec<(usize, usize)> = Vec::new();
    for _ in 0..4 {
        let (own, others, other_bytes, other_runs) = {
            let map = map();
            let (mut own, mut others, mut other_bytes, mut other_runs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for (k, e) in map.range(range_keys(from, to)) {
                let pages = pages_in(k.0, e.pages, from, to);
                if pages == 0 {
                    continue;
                }
                let mut runs = Vec::new();
                for p in 0..16 {
                    if pages >> p & 1 != 0 {
                        let at = k.0 * BLOCK + p * PAGE;
                        runs.push((at.max(from), (at + PAGE).min(to)));
                    }
                }
                if k.1 == me {
                    own.extend(runs);
                    continue;
                }
                let start = e.shadow.span().0 as usize;
                let (a, z) = ((start + e.lo).max(from), (start + e.hi).min(to));
                if a < z {
                    others.push(*k);
                    other_bytes.push((a, z));
                } else {
                    other_runs.push((*k, runs));
                }
            }
            (own, others, other_bytes, other_runs)
        };
        if !others.is_empty() {
            heat(&others);
            land_keys(&others, For::OtherCopy);
            force.extend(other_bytes);
            continue;
        }
        let dirty: Vec<Key> = other_runs.iter().filter(|(_, runs)| !runs.iter().all(|&(a, z)| shadow.journal_clean(a, z))).map(|(k, _)| *k).collect();
        if dirty.is_empty() {
            return (own, force);
        }
        land_keys(&dirty, For::Refresh);
    }
    // (Armed again and again meanwhile: left alone as shaders' ranges are.)
    let map = map();
    let own = map
        .range(range_keys(from, to))
        .filter(|(k, _)| k.1 == me)
        .flat_map(|(k, e)| {
            let pages = pages_in(k.0, e.pages, from, to);
            (0..16).filter(move |p| pages >> p & 1 != 0).map(move |p| {
                let at = k.0 * BLOCK + p * PAGE;
                (at.max(from), (at + PAGE).min(to))
            })
        })
        .collect();
    (own, force)
}

/// Whether another copy than `shadow` has armed bytes in `[from, to)`: a
/// refresh of `shadow` there is due whatever the journal says
/// ([`before_refresh`]).
pub(crate) fn others_over(shadow: &Shadow, from: usize, to: usize) -> bool {
    if !armed_over(from, to) {
        return false;
    }
    let me = shadow.id();
    map().range(range_keys(from, to)).any(|(k, e)| {
        let start = e.shadow.span().0 as usize;
        k.1 != me && start + e.lo < to && start + e.hi > from
    })
}

/// Title memory `[at, at + len)` is freed or given again (`len` 0: the
/// allocation at `at`, whose end `end_of(inside)` says): what is armed there
/// is forgotten, not landed. Its pages go with the memory, or come back
/// read-write with a commit.
pub(crate) fn forget(at: usize, len: usize, end_of: impl FnOnce(usize) -> usize) {
    if !any() {
        return;
    }
    let to = if len != 0 {
        at.saturating_add(len)
    } else {
        let first = map().range((at / BLOCK, 0)..).next().map(|(k, _)| k.0);
        match first {
            Some(b) => end_of((b * BLOCK).max(at)),
            None => return,
        }
    };
    let mut n = 0;
    {
        let _reg = reg();
        let mut map = map_mut();
        let keys: Vec<Key> = map.range(range_keys(at, to)).map(|(&k, _)| k).collect();
        for k in keys {
            let Some(e) = map.get_mut(&k) else {
                continue;
            };
            let pages = pages_in(k.0, e.pages, at, to);
            if pages == 0 {
                continue;
            }
            e.pages &= !pages;
            n += 1;
            if e.pages == 0 {
                map.remove(&k);
                ENTRIES.fetch_sub(1, Ordering::AcqRel);
                if map.range(block_keys(k.0)).next().is_none() {
                    ARMED.clear(k.0);
                }
            }
        }
    }
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
        (0..WHYS).filter(|&i| s[stat::EAGER + i] > 0.0).map(|i| format!("{} {:.1}", WHY_NAMES[i], s[stat::EAGER + i])).collect();
    let kb = |v: f64| v / 1024.0;
    tracing::info!(
        "landing on touch per frame (VICTORY_LAND_LAZY={:?}): armed {:.0} pieces, {:.0} pages newly no-access ({:.0} KB compared \
         with the copies first), {} entries armed now; {:.0} protection calls {:.2} ms; first touches {:.1} reads, {:.1} writes, \
         {:.1} by our copies; title threads in the handler {:.2} ms (longest {:.2} ms); {:.1} landings of {:.1} blocks, {:.0} KB \
         fetched, {:.0} KB put, {:.2} ms on the relay; {:.1} blocks landed for our own readers and writers [{}]; {:.0} traps given \
         up, {:.0} copies of ours failed, {:.0} entries whose memory changed, {:.0} forgotten with their memory; told at the card's \
         completion {:.0}; landed as before: [{}]; armed pieces of copies with others over them: of buffers {:.0}, windows {:.0}, \
         exact copies {:.0} ({} copies since the last line); arms of two copies on the same pages {:.1}, blocks with two copies' \
         arms on other pages {:.1}; access violations shown to us {:.1}, at pages armed once {:.1}; VirtualQuery {:.1} calls \
         {:.2} ms",
        mode(),
        s[stat::ARMED_PIECES],
        s[stat::ARMED_PAGES],
        kb(s[stat::COMPARED]),
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
        (0..7).filter(|&i| s[stat::FOR + i] > 0.0).map(|i| format!("{} {:.1}", FOR_NAMES[i], s[stat::FOR + i])).collect::<Vec<_>>().join(", "),
        s[stat::GAVE_UP],
        s[stat::OURS_FAILED],
        s[stat::LOST],
        s[stat::FORGOTTEN],
        s[stat::TOLD],
        eager.join(", "),
        s[stat::SHARED],
        s[stat::SHARED + 1],
        s[stat::SHARED + 2],
        std::mem::take(&mut reg().shared).len(),
        s[stat::CONFLICT],
        s[stat::BESIDE_COPY],
        s[stat::FAULTS],
        s[stat::FAULTS_OURS],
        s[stat::QUERIES],
        s[stat::QUERY_US] / 1000.0,
    );
    let classes: Vec<String> =
        (0..WHYS).filter(|&i| s[stat::CLASS + i] > 0.0).map(|i| format!("{} {:.1}", WHY_NAMES[i], s[stat::CLASS + i])).collect();
    tracing::info!(
        "landing on touch, telling per frame: {:.1} landings, {:.1} of them fetched from the card before the title was told \
         (in =check: would in =1), for pieces of [{}]; {:.1} with copies in the PC's memory to merge before it",
        s[stat::LANDINGS_ALL],
        s[stat::LANDINGS_FETCH],
        classes.join(", "),
        s[stat::LANDINGS_PC],
    );
    if mode() == Mode::Check {
        tracing::info!(
            "landing on touch, check: at the eager merge {:.0} pieces of armed copies, {:.0} bytes, compared with what a landing \
             on touch would have put: {:.0} differ (must be 0); at a touch, of {:.0} bytes a landing on touch would put, {:.0} \
             differ from title memory (must be 0), {:.0} more in pieces VICTORY_LAND_NOT_KB leaves out (what it would add); not \
             compared: {:.1} landings a flight still writes, {:.1} that the landing as before writes next",
            s[stat::SAMPLE_PIECES],
            s[stat::SAMPLED],
            s[stat::SAMPLE_DIFFER],
            s[stat::CHECKED],
            s[stat::DIFFER],
            s[stat::DIFFER_LEFT_OUT],
            s[stat::IN_FLIGHT],
            s[stat::SUPERSEDED],
        );
    }
}
