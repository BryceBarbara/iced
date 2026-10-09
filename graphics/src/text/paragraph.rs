//! Draw paragraphs.
use crate::core;
use crate::core::alignment;
use crate::core::text::{
    Alignment, Hit, LineHeight, Shaping, Span, Text, Wrapping,
};
use crate::core::{Font, Pixels, Point, Rectangle, Size};
use crate::text;

use std::fmt;
use std::sync::{self, Arc};

/// The memo shards for plain paragraphs (`with_text`).
static PLAIN_MEMO: [text::memo::Shard<Internal>; text::memo::SHARDS] =
    text::memo::new_shards();

/// The memo shards for rich paragraphs (`with_spans`).
static RICH_MEMO: [text::memo::Shard<Internal>; text::memo::SHARDS] =
    text::memo::new_shards();

/// The shards a key belongs to.
fn memo_of(
    key: &text::memo::Key,
) -> &'static [text::memo::Shard<Internal>; text::memo::SHARDS] {
    if key.is_rich() {
        &RICH_MEMO
    } else {
        &PLAIN_MEMO
    }
}

/// A bunch of text.
#[derive(Clone, PartialEq)]
pub struct Paragraph(Arc<Internal>);

#[derive(Clone)]
struct Internal {
    /// The memo key this shape was produced for, or `None` for a paragraph
    /// nothing keyed (`Paragraph::default`). `resize` derives the resized
    /// key from it, which is what lets a resize hit the memo.
    key: Option<Arc<text::memo::Key>>,
    buffer: cosmic_text::Buffer,
    font: Font,
    shaping: Shaping,
    wrapping: Wrapping,
    align_x: Alignment,
    align_y: alignment::Vertical,
    bounds: Size,
    min_bounds: Size,
    version: text::Version,
}

impl Paragraph {
    /// Creates a new empty [`Paragraph`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the buffer of the [`Paragraph`].
    pub fn buffer(&self) -> &cosmic_text::Buffer {
        &self.internal().buffer
    }

    /// Creates a [`Weak`] reference to the [`Paragraph`].
    ///
    /// This is useful to avoid cloning the [`Paragraph`] when
    /// referential guarantees are unnecessary. For instance,
    /// when creating a rendering tree.
    pub fn downgrade(&self) -> Weak {
        let paragraph = self.internal();

        Weak {
            raw: Arc::downgrade(paragraph),
            min_bounds: paragraph.min_bounds,
            align_x: paragraph.align_x,
            align_y: paragraph.align_y,
        }
    }

    fn internal(&self) -> &Arc<Internal> {
        &self.0
    }
}

impl core::text::Paragraph for Paragraph {
    type Font = Font;

    fn with_text(text: Text<&str>) -> Self {
        let key = text::memo::Key::plain(&text);

        Self(text::memo::get_or_shape(&PLAIN_MEMO, key, |key| {
            shape_text(text, key)
        }))
    }

    fn with_spans<Link>(text: Text<&[Span<'_, Link>]>) -> Self {
        let key = text::memo::Key::rich(&text);

        Self(text::memo::get_or_shape(&RICH_MEMO, key, |key| {
            shape_spans(text, key)
        }))
    }

    fn resize(&mut self, new_bounds: Size) {
        let Some(key) = self.0.key.clone() else {
            // Nothing keyed this paragraph, so there is nothing to look up.
            resize_in_place(Arc::make_mut(&mut self.0), new_bounds);

            return;
        };

        let resized = key.resized(new_bounds);

        // The memo may already hold this paragraph at these bounds, and then
        // the resize costs neither the font system's lock nor the buffer
        // clone `Arc::make_mut` owes a shape the memo is still holding.
        if let Some(hit) = text::memo::get(memo_of(&resized), &resized) {
            self.0 = hit;

            return;
        }

        let resized = Arc::new(resized);
        let paragraph = Arc::make_mut(&mut self.0);

        resize_in_place(paragraph, new_bounds);
        paragraph.key = Some(Arc::clone(&resized));

        text::memo::insert(memo_of(&resized), resized, Arc::clone(&self.0));
    }

    fn compare(&self, text: Text<()>) -> core::text::Difference {
        let paragraph = self.internal();
        let metrics = paragraph.buffer.metrics();

        // The font system's version is read from its atomic mirror, not from
        // under `font_system().read()`: this runs on every layout pass of
        // every text widget, and a reader still queues behind a writer.
        if paragraph.version.0 != text::memo::font_version()
            || metrics.font_size != text.size.0
            || metrics.line_height != text.line_height.to_absolute(text.size).0
            || paragraph.font != text.font
            || paragraph.shaping != text.shaping
            || paragraph.wrapping != text.wrapping
            || paragraph.align_x != text.align_x
            || paragraph.align_y != text.align_y
        {
            core::text::Difference::Shape
        } else if paragraph.bounds != text.bounds {
            core::text::Difference::Bounds
        } else {
            core::text::Difference::None
        }
    }

    fn size(&self) -> Pixels {
        Pixels(self.0.buffer.metrics().font_size)
    }

    fn font(&self) -> Font {
        self.0.font
    }

    fn line_height(&self) -> LineHeight {
        LineHeight::Absolute(Pixels(self.0.buffer.metrics().line_height))
    }

    fn align_x(&self) -> Alignment {
        self.internal().align_x
    }

    fn align_y(&self) -> alignment::Vertical {
        self.internal().align_y
    }

    fn wrapping(&self) -> Wrapping {
        self.0.wrapping
    }

    fn shaping(&self) -> Shaping {
        self.0.shaping
    }

    fn bounds(&self) -> Size {
        self.0.bounds
    }

    fn min_bounds(&self) -> Size {
        self.internal().min_bounds
    }

    fn hit_test(&self, point: Point) -> Option<Hit> {
        let cursor = self.internal().buffer.hit(point.x, point.y)?;

        Some(Hit::CharOffset(cursor.index))
    }

    fn hit_span(&self, point: Point) -> Option<usize> {
        let internal = self.internal();

        let cursor = internal.buffer.hit(point.x, point.y)?;
        let line = internal.buffer.lines.get(cursor.line)?;

        if cursor.index >= line.text().len() {
            return None;
        }

        let index = match cursor.affinity {
            cosmic_text::Affinity::Before => cursor.index.saturating_sub(1),
            cosmic_text::Affinity::After => cursor.index,
        };

        let mut hit = None;
        let glyphs = line
            .layout_opt()
            .as_ref()?
            .iter()
            .flat_map(|line| line.glyphs.iter());

        for glyph in glyphs {
            if glyph.start <= index && index < glyph.end {
                hit = Some(glyph);
                break;
            }
        }

        Some(hit?.metadata)
    }

    fn span_bounds(&self, index: usize) -> Vec<Rectangle> {
        let internal = self.internal();

        let mut bounds = Vec::new();
        let mut current_bounds = None;

        let glyphs = internal
            .buffer
            .layout_runs()
            .flat_map(|run| {
                let line_top = run.line_top;
                let line_height = run.line_height;

                run.glyphs
                    .iter()
                    .map(move |glyph| (line_top, line_height, glyph))
            })
            .skip_while(|(_, _, glyph)| glyph.metadata != index)
            .take_while(|(_, _, glyph)| glyph.metadata == index);

        for (line_top, line_height, glyph) in glyphs {
            let y = line_top + glyph.y;

            let new_bounds = || {
                Rectangle::new(
                    Point::new(glyph.x, y),
                    Size::new(
                        glyph.w,
                        glyph.line_height_opt.unwrap_or(line_height),
                    ),
                )
            };

            match current_bounds.as_mut() {
                None => {
                    current_bounds = Some(new_bounds());
                }
                Some(current_bounds) if y != current_bounds.y => {
                    bounds.push(*current_bounds);
                    *current_bounds = new_bounds();
                }
                Some(current_bounds) => {
                    current_bounds.width += glyph.w;
                }
            }
        }

        bounds.extend(current_bounds);
        bounds
    }

    fn grapheme_position(&self, line: usize, index: usize) -> Option<Point> {
        use unicode_segmentation::UnicodeSegmentation;

        let run = self.internal().buffer.layout_runs().nth(line)?;

        // index represents a grapheme, not a glyph
        // Let's find the first glyph for the given grapheme cluster
        let mut last_start = None;
        let mut last_grapheme_count = 0;
        let mut graphemes_seen = 0;

        let glyph = run
            .glyphs
            .iter()
            .find(|glyph| {
                if Some(glyph.start) != last_start {
                    last_grapheme_count = run.text[glyph.start..glyph.end]
                        .graphemes(false)
                        .count();
                    last_start = Some(glyph.start);
                    graphemes_seen += last_grapheme_count;
                }

                graphemes_seen >= index
            })
            .or_else(|| run.glyphs.last())?;

        let advance = if index == 0 {
            0.0
        } else {
            glyph.w
                * (1.0
                    - graphemes_seen.saturating_sub(index) as f32
                        / last_grapheme_count.max(1) as f32)
        };

        Some(Point::new(
            glyph.x + glyph.x_offset * glyph.font_size + advance,
            glyph.y - glyph.y_offset * glyph.font_size,
        ))
    }
}

impl Default for Paragraph {
    fn default() -> Self {
        Self(Arc::new(Internal::default()))
    }
}

impl fmt::Debug for Paragraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let paragraph = self.internal();

        f.debug_struct("Paragraph")
            .field("font", &paragraph.font)
            .field("shaping", &paragraph.shaping)
            .field("horizontal_alignment", &paragraph.align_x)
            .field("vertical_alignment", &paragraph.align_y)
            .field("bounds", &paragraph.bounds)
            .field("min_bounds", &paragraph.min_bounds)
            .finish()
    }
}

impl PartialEq for Internal {
    fn eq(&self, other: &Self) -> bool {
        self.font == other.font
            && self.shaping == other.shaping
            && self.align_x == other.align_x
            && self.align_y == other.align_y
            && self.bounds == other.bounds
            && self.min_bounds == other.min_bounds
            && self.buffer.metrics() == other.buffer.metrics()
    }
}

impl Default for Internal {
    fn default() -> Self {
        Self {
            key: None,
            buffer: cosmic_text::Buffer::new_empty(cosmic_text::Metrics {
                font_size: 1.0,
                line_height: 1.0,
            }),
            font: Font::default(),
            shaping: Shaping::default(),
            wrapping: Wrapping::default(),
            align_x: Alignment::Default,
            align_y: alignment::Vertical::Top,
            bounds: Size::ZERO,
            min_bounds: Size::ZERO,
            version: text::Version::default(),
        }
    }
}

/// A weak reference to a [`Paragraph`].
#[derive(Debug, Clone)]
pub struct Weak {
    raw: sync::Weak<Internal>,
    /// The minimum bounds of the [`Paragraph`].
    pub min_bounds: Size,
    /// The horizontal alignment of the [`Paragraph`].
    pub align_x: Alignment,
    /// The vertical alignment of the [`Paragraph`].
    pub align_y: alignment::Vertical,
}

impl Weak {
    /// Tries to update the reference into a [`Paragraph`].
    pub fn upgrade(&self) -> Option<Paragraph> {
        self.raw.upgrade().map(Paragraph)
    }
}

impl PartialEq for Weak {
    fn eq(&self, other: &Self) -> bool {
        match (self.raw.upgrade(), other.raw.upgrade()) {
            (Some(p1), Some(p2)) => p1 == p2,
            _ => false,
        }
    }
}

/// Re-breaks `paragraph`'s lines at `new_bounds`, under the global lock.
fn resize_in_place(paragraph: &mut Internal, new_bounds: Size) {
    let mut font_system =
        text::font_system().write().expect("Write font system");

    paragraph.buffer.set_size(
        font_system.raw(),
        Some(new_bounds.width),
        Some(new_bounds.height),
    );

    let min_bounds = text::align(
        &mut paragraph.buffer,
        font_system.raw(),
        paragraph.align_x,
    );

    paragraph.bounds = new_bounds;
    paragraph.min_bounds = min_bounds;
}

fn shape_text(text: Text<&str>, key: Arc<text::memo::Key>) -> Arc<Internal> {
    log::trace!("Allocating plain paragraph: {}", text.content);

    let mut font_system =
        text::font_system().write().expect("Write font system");

    let mut buffer = cosmic_text::Buffer::new(
        font_system.raw(),
        cosmic_text::Metrics::new(
            text.size.into(),
            text.line_height.to_absolute(text.size).into(),
        ),
    );

    buffer.set_size(
        font_system.raw(),
        Some(text.bounds.width),
        Some(text.bounds.height),
    );

    buffer.set_wrap(font_system.raw(), text::to_wrap(text.wrapping));

    buffer.set_text(
        font_system.raw(),
        text.content,
        &text::to_attributes(text.font),
        text::to_shaping(text.shaping, text.content),
        None,
    );

    let min_bounds = text::align(&mut buffer, font_system.raw(), text.align_x);

    Arc::new(Internal {
        key: Some(key),
        buffer,
        font: text.font,
        align_x: text.align_x,
        align_y: text.align_y,
        shaping: text.shaping,
        wrapping: text.wrapping,
        bounds: text.bounds,
        min_bounds,
        version: font_system.version(),
    })
}

fn shape_spans<Link>(
    text: Text<&[Span<'_, Link>]>,
    key: Arc<text::memo::Key>,
) -> Arc<Internal> {
    log::trace!("Allocating rich paragraph: {} spans", text.content.len());

    let mut font_system =
        text::font_system().write().expect("Write font system");

    let mut buffer = cosmic_text::Buffer::new(
        font_system.raw(),
        cosmic_text::Metrics::new(
            text.size.into(),
            text.line_height.to_absolute(text.size).into(),
        ),
    );

    buffer.set_size(
        font_system.raw(),
        Some(text.bounds.width),
        Some(text.bounds.height),
    );

    buffer.set_wrap(font_system.raw(), text::to_wrap(text.wrapping));

    buffer.set_rich_text(
        font_system.raw(),
        text.content.iter().enumerate().map(|(i, span)| {
            let attrs = text::to_attributes(span.font.unwrap_or(text.font));

            let attrs = match (span.size, span.line_height) {
                (None, None) => attrs,
                _ => {
                    let size = span.size.unwrap_or(text.size);

                    attrs.metrics(cosmic_text::Metrics::new(
                        size.into(),
                        span.line_height
                            .unwrap_or(text.line_height)
                            .to_absolute(size)
                            .into(),
                    ))
                }
            };

            let attrs = if let Some(color) = span.color {
                attrs.color(text::to_color(color))
            } else {
                attrs
            };

            (span.text.as_ref(), attrs.metadata(i))
        }),
        &text::to_attributes(text.font),
        cosmic_text::Shaping::Advanced,
        None,
    );

    let min_bounds = text::align(&mut buffer, font_system.raw(), text.align_x);

    Arc::new(Internal {
        key: Some(key),
        buffer,
        font: text.font,
        align_x: text.align_x,
        align_y: text.align_y,
        shaping: text.shaping,
        wrapping: text.wrapping,
        bounds: text.bounds,
        min_bounds,
        version: font_system.version(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::text::Paragraph as _;

    fn text(content: &str, width: f32) -> Text<&str> {
        Text {
            content,
            bounds: Size::new(width, 100.0),
            size: Pixels(16.0),
            line_height: LineHeight::default(),
            font: Font::default(),
            align_x: Alignment::Default,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::default(),
            wrapping: Wrapping::default(),
        }
    }

    /// `resize` must go through the memo rather than through
    /// `Arc::make_mut`, which deep-clones the whole `cosmic_text::Buffer`
    /// whenever the memo is still holding the shape being resized -- i.e. on
    /// every resize of a paragraph the memo just answered.
    ///
    /// Observable as pointer identity: a resize that cloned would leave the
    /// two paragraphs on two different allocations.
    #[test]
    fn a_resize_after_a_memo_hit_shares_the_resized_shape() {
        let resized = Size::new(123.0, 45.0);
        let shape = text("a resize must not deep-clone this buffer", 600.0);

        let mut first = Paragraph::with_text(shape);
        first.resize(resized);

        // A second identical shape is a memo HIT, so `second` and `first`
        // start life on the same allocation -- which is exactly the state
        // that used to make the resize below deep-clone.
        let mut second = Paragraph::with_text(shape);
        let mut third = Paragraph::with_text(shape);

        second.resize(resized);
        third.resize(resized);

        assert!(
            Arc::ptr_eq(&second.0, &third.0),
            "two identical resizes must land on one memoized buffer"
        );
        assert!(
            Arc::ptr_eq(&first.0, &second.0),
            "the resize of a memo hit must come from the memo, not a clone"
        );
        assert_eq!(second.bounds(), resized);
        assert_eq!(second.min_bounds(), first.min_bounds());
    }

    /// The resized shape is keyed by the bounds it was resized TO, so a
    /// different target is a different entry.
    #[test]
    fn resizing_to_different_bounds_does_not_share() {
        let shape = text("two targets", 600.0);

        let mut narrow = Paragraph::with_text(shape);
        let mut wide = Paragraph::with_text(shape);

        narrow.resize(Size::new(20.0, 40.0));
        wide.resize(Size::new(500.0, 40.0));

        assert!(!Arc::ptr_eq(&narrow.0, &wide.0));
        assert_eq!(narrow.bounds(), Size::new(20.0, 40.0));
        assert_eq!(wide.bounds(), Size::new(500.0, 40.0));
    }

    /// A memo hit has to be indistinguishable from the miss that filled it.
    #[test]
    fn a_hit_measures_the_same_as_the_miss_that_filled_it() {
        let shape = text("the hit and the miss agree", 400.0);

        let miss = Paragraph::with_text(shape);
        let hit = Paragraph::with_text(shape);

        assert_eq!(miss.min_bounds(), hit.min_bounds());
        assert_eq!(miss.bounds(), hit.bounds());
        assert_eq!(
            miss.buffer()
                .layout_runs()
                .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.start))
                .collect::<Vec<_>>(),
            hit.buffer()
                .layout_runs()
                .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.start))
                .collect::<Vec<_>>(),
        );
    }

    /// Any input the shaper is given has to be part of the key.
    #[test]
    fn the_key_separates_every_shaping_input() {
        let base = text("distinct", 400.0);

        let variants = [
            Text {
                size: Pixels(32.0),
                ..base
            },
            Text {
                bounds: Size::new(10.0, 100.0),
                ..base
            },
            Text {
                align_x: Alignment::Center,
                ..base
            },
            Text {
                align_y: alignment::Vertical::Bottom,
                ..base
            },
            Text {
                wrapping: Wrapping::None,
                ..base
            },
            Text {
                line_height: LineHeight::Absolute(Pixels(40.0)),
                ..base
            },
            Text {
                shaping: Shaping::Basic,
                ..base
            },
        ];

        for variant in variants {
            assert!(
                !Arc::ptr_eq(
                    &Paragraph::with_text(base).0,
                    &Paragraph::with_text(variant).0
                ),
                "a changed shaping input must miss the memo"
            );
        }
    }
}
