//! A shape memo in front of the process-global [`FontSystem`] write lock.
//!
//! [`Paragraph::with_text`] and [`Paragraph::with_spans`] take
//! `text::font_system().write()` across `Buffer::new` + `set_size` +
//! `set_wrap` + `set_text` + `align`, so every layout pass on every thread
//! queues on one writer. This module keys the *inputs* of that work and hands
//! back an `Arc` clone of a previous result, so a repeated shape takes no lock
//! at all: a lookup is a sharded `RwLock::read`, which readers hold
//! concurrently.
//!
//! Three properties make it safe to keep on unconditionally:
//!
//! * **The key covers every input `cosmic_text` is given**, including the
//!   [`FontSystem`]'s version, so loading a font retires every entry shaped
//!   before it.
//! * **Recency never needs a write.** A hit marks its entry `used` with one
//!   relaxed atomic store, which `&Entry` allows under the read lock. An
//!   eviction sweep then keeps what has been used since the last sweep -- the
//!   same second-chance policy as [`crate::text::Cache::trim`], which is what
//!   iced already does for the renderer-side text cache.
//! * **It is bounded.** [`SHARDS`] * `SHARD_CAPACITY` entries, and a sweep
//!   always leaves a shard at most half full, so the bound holds even when
//!   every entry claims recent use.
//!
//! [`FontSystem`]: crate::text::FontSystem
//! [`Paragraph::with_text`]: crate::text::Paragraph
//! [`Paragraph::with_spans`]: crate::text::Paragraph

use crate::core::alignment;
use crate::core::text::{Alignment, LineHeight, Shaping, Wrapping};
use crate::core::{Color, Font, Pixels, Size};

use rustc_hash::FxBuildHasher;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// The current [`crate::text::Version`] of the global font system, mirrored
/// into an atomic so neither a memo hit nor a paragraph comparison needs a
/// lock on the font system itself.
///
/// `FontSystem::load_font` bumps both, under the write lock it already holds.
static FONT_VERSION: AtomicU32 = AtomicU32::new(0);

/// Records that the global font system's version advanced.
pub(crate) fn bump_font_version(version: u32) {
    FONT_VERSION.store(version, Ordering::Release);
}

/// Returns the mirrored font system version.
pub(crate) fn font_version() -> u32 {
    FONT_VERSION.load(Ordering::Acquire)
}

/// The file the running totals are appended to, named by
/// `WB_ICED_TEXT_MEMO_STATS`. Unset -- the normal case -- costs one
/// already-initialized `OnceLock` read per lookup and nothing else.
///
/// A FILE and not stderr because `libtest` captures a test thread's
/// `eprintln!` and throws it away unless the test fails.
fn stats_sink() -> Option<&'static str> {
    static SINK: OnceLock<Option<String>> = OnceLock::new();

    SINK.get_or_init(|| std::env::var("WB_ICED_TEXT_MEMO_STATS").ok())
        .as_deref()
}

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static EVICTED: AtomicU64 = AtomicU64::new(0);
static HERD: AtomicU64 = AtomicU64::new(0);

/// How often the running totals are appended under `WB_ICED_TEXT_MEMO_STATS`.
const STATS_EVERY: u64 = 20_000;

/// Returns the running `(hits, misses)` totals of the memo.
pub fn stats() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}

/// Records that an eviction sweep dropped `count` entries.
fn record_eviction(count: usize) {
    if stats_sink().is_none() {
        return;
    }

    let _ = EVICTED.fetch_add(count as u64, Ordering::Relaxed);
}

/// Records a miss that shaped a key another thread had already shaped: work
/// the lock serialized twice.
fn record_herd() {
    if stats_sink().is_none() {
        return;
    }

    let _ = HERD.fetch_add(1, Ordering::Relaxed);
}

fn record(hit: bool) {
    let Some(sink) = stats_sink() else {
        return;
    };

    let _ = if hit {
        HITS.fetch_add(1, Ordering::Relaxed)
    } else {
        MISSES.fetch_add(1, Ordering::Relaxed)
    };

    let (hits, misses) = stats();

    if (hits + misses) % STATS_EVERY != 0 {
        return;
    }

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sink)
    {
        use std::io::Write;

        let _ = writeln!(
            file,
            "hits={hits} misses={misses} rate={:.1}% \
             evicted={evicted} herd={herd}",
            100.0 * hits as f64 / (hits + misses).max(1) as f64,
            evicted = EVICTED.load(Ordering::Relaxed),
            herd = HERD.load(Ordering::Relaxed),
        );
    }
}

/// A `f32` keyed by its bit pattern, so `NaN` and `-0.0` are distinct keys
/// rather than unhashable or wrongly-equal ones.
type Bits = u32;

fn bits(value: f32) -> Bits {
    value.to_bits()
}

fn line_height_key(line_height: LineHeight) -> (u8, Bits) {
    match line_height {
        LineHeight::Absolute(Pixels(px)) => (0, bits(px)),
        LineHeight::Relative(factor) => (1, bits(factor)),
    }
}

/// The shaping inputs of one span of a rich paragraph.
///
/// Only the fields `with_spans` actually feeds to `set_rich_text` are keyed:
/// text, font, size, line height and colour. `link`, `highlight`, `padding`,
/// `underline` and `strikethrough` never reach `cosmic_text`, so they cannot
/// change the shaped result.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SpanKey {
    text: String,
    font: Option<Font>,
    size: Option<Bits>,
    line_height: Option<(u8, Bits)>,
    color: Option<[u8; 4]>,
}

impl SpanKey {
    pub(crate) fn new<Link>(span: &crate::core::text::Span<'_, Link>) -> Self {
        Self {
            text: span.text.to_string(),
            font: span.font,
            size: span.size.map(|Pixels(px)| bits(px)),
            line_height: span.line_height.map(line_height_key),
            color: span.color.map(Color::into_rgba8),
        }
    }
}

/// Which shaping path produced an entry, so two results that could share a
/// content string never share a key.
///
/// `Rich` is separate from `Plain` because `with_spans` always asks
/// `cosmic_text` for `Shaping::Advanced` and carries per-span metadata, so
/// the two are not interchangeable even for one span of plain text.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Plain,
    Rich,
}

/// Every input that can change a shaped paragraph.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    kind: Kind,
    /// Set on an entry produced by `resize` -- `set_size` + `align` over an
    /// existing buffer -- so a fresh shape is never answered by one, and the
    /// two key spaces cannot contaminate each other.
    resized: bool,
    content: String,
    spans: Vec<SpanKey>,
    font: Font,
    size: Bits,
    line_height: (u8, Bits),
    bounds: (Bits, Bits),
    align_x: Alignment,
    align_y: alignment::Vertical,
    shaping: Shaping,
    wrapping: Wrapping,
    version: u32,
}

impl Key {
    /// Builds the key of a plain paragraph.
    pub(crate) fn plain(text: &crate::core::text::Text<&str>) -> Self {
        Self {
            kind: Kind::Plain,
            content: text.content.to_owned(),
            ..Self::common(
                text.font,
                text.size,
                text.line_height,
                text.bounds,
                text.align_x,
                text.align_y,
                text.shaping,
                text.wrapping,
            )
        }
    }

    /// Builds the key of a rich paragraph.
    pub(crate) fn rich<Link>(
        text: &crate::core::text::Text<&[crate::core::text::Span<'_, Link>]>,
    ) -> Self {
        Self {
            kind: Kind::Rich,
            spans: text.content.iter().map(SpanKey::new).collect(),
            ..Self::common(
                text.font,
                text.size,
                text.line_height,
                text.bounds,
                text.align_x,
                text.align_y,
                text.shaping,
                text.wrapping,
            )
        }
    }

    /// Builds the key the same paragraph has once it is resized to `bounds`.
    ///
    /// `set_size` + `align` re-break the lines of a buffer whose text, attrs
    /// and metrics are unchanged, so the result depends on the new bounds and
    /// not on the bounds the paragraph was first shaped at -- which is why
    /// this overwrites the original bounds instead of keying both.
    pub(crate) fn resized(&self, bounds: Size) -> Self {
        Self {
            resized: true,
            bounds: (bits(bounds.width), bits(bounds.height)),
            ..self.clone()
        }
    }

    /// Whether this key belongs to a rich paragraph's key space.
    pub(crate) fn is_rich(&self) -> bool {
        self.kind == Kind::Rich
    }

    #[allow(clippy::too_many_arguments)]
    fn common(
        font: Font,
        size: Pixels,
        line_height: LineHeight,
        bounds: Size,
        align_x: Alignment,
        align_y: alignment::Vertical,
        shaping: Shaping,
        wrapping: Wrapping,
    ) -> Self {
        Self {
            kind: Kind::Plain,
            resized: false,
            content: String::new(),
            spans: Vec::new(),
            font,
            size: bits(size.0),
            line_height: line_height_key(line_height),
            bounds: (bits(bounds.width), bits(bounds.height)),
            align_x,
            align_y,
            shaping,
            wrapping,
            version: font_version(),
        }
    }
}

/// How many shards the memo is split across, so a lookup rarely contends.
pub(crate) const SHARDS: usize = 32;

/// How many entries one shard holds before an eviction sweep runs.
///
/// `SHARDS * SHARD_CAPACITY` bounds the memo at 16384 paragraphs, and a sweep
/// leaves a shard at most half full, so the bound holds even when every entry
/// claims recent use. An entry a live paragraph also holds costs the memo one
/// pointer, not a second buffer.
const SHARD_CAPACITY: usize = 512;

/// A memoized shape, plus the one bit an eviction sweep reads.
///
/// `used` is an atomic so a HIT can set it through the `&Entry` a read lock
/// hands out: making recency a write would put every lookup back behind a
/// writer, which is the cost this module exists to remove.
pub(crate) struct Entry<T> {
    value: Arc<T>,
    used: AtomicBool,
}

pub(crate) type Shard<T> = RwLock<HashMap<Arc<Key>, Entry<T>, FxBuildHasher>>;

/// Builds the shard array backing one memo, usable in a `static`.
pub(crate) const fn new_shards<T>() -> [Shard<T>; SHARDS] {
    [const { RwLock::new(HashMap::with_hasher(FxBuildHasher)) }; SHARDS]
}

/// Looks `key` up without shaping anything.
pub(crate) fn get<T>(
    shards: &'static [Shard<T>; SHARDS],
    key: &Key,
) -> Option<Arc<T>> {
    let shard = &shards[shard_of(key)];
    let read = shard.read().expect("Read paragraph memo");

    let value = read.get(key).map(|entry| {
        entry.used.store(true, Ordering::Relaxed);

        Arc::clone(&entry.value)
    });

    drop(read);
    record(value.is_some());

    value
}

/// Publishes `value` under `key`, sweeping first if the shard is full.
pub(crate) fn insert<T>(
    shards: &'static [Shard<T>; SHARDS],
    key: Arc<Key>,
    value: Arc<T>,
) {
    // A font loaded while this was being shaped retires the key before it is
    // ever published: the result belongs to a version the key does not name.
    if key.version != font_version() {
        return;
    }

    let mut write = shards[shard_of(&key)]
        .write()
        .expect("Write paragraph memo");

    if write.len() >= SHARD_CAPACITY {
        sweep(&mut write);
    }

    let entry = Entry {
        value,
        used: AtomicBool::new(true),
    };

    if write.insert(key, entry).is_some() {
        record_herd();
    }
}

/// Drops the entries of one shard that have not been used since the last
/// sweep, leaving the shard at most half full either way.
fn sweep<T>(map: &mut HashMap<Arc<Key>, Entry<T>, FxBuildHasher>) {
    let before = map.len();
    let mut kept = 0;

    map.retain(|_, entry| {
        let keep = entry.used.swap(false, Ordering::Relaxed)
            && kept < SHARD_CAPACITY / 2;

        if keep {
            kept += 1;
        }

        keep
    });

    record_eviction(before - map.len());
}

/// Looks `key` up, calling `shape` only on a miss.
///
/// `shape` is the original body of `with_text` / `with_spans`, lock and all,
/// and is handed the key so the result can remember what produced it. On a
/// hit nothing touches `font_system()`.
pub(crate) fn get_or_shape<T, F>(
    shards: &'static [Shard<T>; SHARDS],
    key: Key,
    shape: F,
) -> Arc<T>
where
    F: FnOnce(Arc<Key>) -> Arc<T>,
{
    if let Some(hit) = get(shards, &key) {
        return hit;
    }

    let key = Arc::new(key);
    let shaped = shape(Arc::clone(&key));

    insert(shards, key, Arc::clone(&shaped));

    shaped
}

fn shard_of(key: &Key) -> usize {
    use std::hash::{BuildHasher, RandomState};

    static HASHER: OnceLock<RandomState> = OnceLock::new();

    HASHER.get_or_init(RandomState::new).hash_one(key) as usize % SHARDS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(content: &str, width: f32) -> Key {
        Key::plain(&crate::core::text::Text {
            content,
            bounds: Size::new(width, 100.0),
            size: Pixels(16.0),
            line_height: LineHeight::default(),
            font: Font::default(),
            align_x: Alignment::Default,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::default(),
            wrapping: Wrapping::default(),
        })
    }

    fn entry(value: u32, used: bool) -> Entry<u32> {
        Entry {
            value: Arc::new(value),
            used: AtomicBool::new(used),
        }
    }

    /// The eviction policy, which is the only thing standing between an
    /// unconditional memo and an unbounded one: a sweep keeps what has been
    /// used since the last sweep, and leaves the shard at most half full
    /// whatever the answer is.
    #[test]
    fn a_sweep_keeps_what_was_used_and_halves_the_shard() {
        let mut map = HashMap::with_hasher(FxBuildHasher);

        for i in 0..SHARD_CAPACITY {
            let _ = map.insert(
                Arc::new(key("sweep", i as f32)),
                entry(i as u32, i % 2 == 0),
            );
        }

        sweep(&mut map);

        assert_eq!(
            map.len(),
            SHARD_CAPACITY / 2,
            "half the entries were used, so half survive"
        );
        assert!(
            map.values().all(|entry| *entry.value % 2 == 0),
            "an entry that was not used since the last sweep is gone"
        );

        // Nothing was touched since, so the next sweep takes the rest: the
        // survivors' `used` flag was cleared by the sweep that kept them.
        sweep(&mut map);

        assert!(map.is_empty(), "a second chance is not a third one");
    }

    /// Every entry claiming recent use must still not grow the shard.
    #[test]
    fn a_sweep_bounds_a_shard_whose_every_entry_is_in_use() {
        let mut map = HashMap::with_hasher(FxBuildHasher);

        for i in 0..SHARD_CAPACITY * 2 {
            let _ = map
                .insert(Arc::new(key("busy", i as f32)), entry(i as u32, true));
        }

        sweep(&mut map);

        assert_eq!(map.len(), SHARD_CAPACITY / 2);
    }

    /// A hit must mark its entry, or the next sweep drops the hottest shape
    /// in the process.
    #[test]
    fn a_hit_marks_its_entry_used() {
        let shards: &'static [Shard<u32>; SHARDS] =
            Box::leak(Box::new(new_shards()));

        let key = key("marked", 1.0);

        insert(shards, Arc::new(key.clone()), Arc::new(7));

        {
            let shard =
                shards[shard_of(&key)].write().expect("Write test shard");

            shard
                .get(&key)
                .expect("The entry was just inserted")
                .used
                .store(false, Ordering::Relaxed);
        }

        assert_eq!(get(shards, &key).as_deref(), Some(&7));

        let shard = shards[shard_of(&key)].read().expect("Read test shard");

        assert!(
            shard
                .get(&key)
                .expect("Still there")
                .used
                .load(Ordering::Relaxed),
            "the hit should have set the entry's second-chance bit"
        );
    }

    /// Resizing moves an entry into its own key space, so a fresh shape can
    /// never be served a buffer that was produced by `set_size` + `align`.
    #[test]
    fn a_resized_key_is_not_a_shaped_key() {
        let shaped = key("content", 10.0);

        assert!(shaped != shaped.resized(Size::new(10.0, 100.0)));
    }
}
