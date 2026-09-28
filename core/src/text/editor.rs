//! Edit text.
use crate::text::highlighter::{self, Highlighter};
use crate::text::{LineHeight, Wrapping};
use crate::{Pixels, Point, Rectangle, Size};

use std::borrow::Cow;
use std::sync::Arc;

/// A component that can be used by widgets to edit multi-line text.
pub trait Editor: Sized + Default {
    /// The font of the [`Editor`].
    type Font: Copy + PartialEq + Default;

    /// Creates a new [`Editor`] laid out with the given text.
    fn with_text(text: &str) -> Self;

    /// Returns true if the [`Editor`] has no contents.
    fn is_empty(&self) -> bool;

    /// Returns the current [`Cursor`] of the [`Editor`].
    fn cursor(&self) -> Cursor;

    /// Returns the current [`Selection`] of the [`Editor`].
    fn selection(&self) -> Selection;

    /// Returns the current selected text of the [`Editor`].
    fn copy(&self) -> Option<String>;

    /// Returns the text of the given line in the [`Editor`], if it exists.
    fn line(&self, index: usize) -> Option<Line<'_>>;

    /// Returns the amount of lines in the [`Editor`].
    fn line_count(&self) -> usize;

    /// Performs an [`Action`] on the [`Editor`].
    fn perform(&mut self, action: Action);

    /// Replaces the contents of the [`Editor`], keeping the size it was last
    /// laid out at.
    ///
    /// Unlike [`with_text`](Self::with_text) this reuses the editor, so a
    /// caller that rebuilds its contents -- a log or console view appending
    /// lines -- can put the cursor and the [`Scroll`] position back afterwards
    /// instead of starting over at the top. The cursor and selection are reset,
    /// since the text they pointed into is gone.
    fn set_text(&mut self, text: &str);

    /// Returns the current [`Scroll`] position of the [`Editor`].
    ///
    /// This is the position last APPLIED, which an `Action::Scroll` leaves
    /// un-clamped until the next shaping pass: a wheel event past the end
    /// reads back as an offset far beyond the line it names. Pass it through
    /// [`scroll_to`](Self::scroll_to) to settle it first if you mean to
    /// compare it with anything.
    fn scroll(&self) -> Scroll;

    /// Scrolls the [`Editor`] to the given [`Scroll`] position, clamped to what
    /// its contents and size allow.
    fn scroll_to(&mut self, scroll: Scroll);

    /// Returns the [`Scroll`] position that shows the END of the [`Editor`]'s
    /// contents.
    ///
    /// Comparing it with [`scroll`](Self::scroll) is how a caller asks "is the
    /// view at the bottom"; passing it to [`scroll_to`](Self::scroll_to) is how
    /// a log view follows its own tail. Shaping is lazy, so this may shape
    /// lines that have not been shaped yet; the current scroll position is left
    /// as it was.
    fn end_scroll(&mut self) -> Scroll;

    /// Moves the cursor to the given position.
    fn move_to(&mut self, cursor: Cursor);

    /// Returns the current boundaries of the [`Editor`].
    fn bounds(&self) -> Size;

    /// Returns the minimum boundaries to fit the current contents of
    /// the [`Editor`].
    fn min_bounds(&self) -> Size;

    /// Updates the [`Editor`] with some new attributes.
    fn update(
        &mut self,
        new_bounds: Size,
        new_font: Self::Font,
        new_size: Pixels,
        new_line_height: LineHeight,
        new_wrapping: Wrapping,
        new_highlighter: &mut impl Highlighter,
    );

    /// Runs a text [`Highlighter`] in the [`Editor`].
    fn highlight<H: Highlighter>(
        &mut self,
        font: Self::Font,
        highlighter: &mut H,
        format_highlight: impl Fn(&H::Highlight) -> highlighter::Format<Self::Font>,
    );
}

/// An interaction with an [`Editor`].
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Apply a [`Motion`].
    Move(Motion),
    /// Select text with a given [`Motion`].
    Select(Motion),
    /// Select the word at the current cursor.
    SelectWord,
    /// Select the line at the current cursor.
    SelectLine,
    /// Select the entire buffer.
    SelectAll,
    /// Perform an [`Edit`].
    Edit(Edit),
    /// Click the [`Editor`] at the given [`Point`].
    Click(Point),
    /// Drag the mouse on the [`Editor`] to the given [`Point`].
    Drag(Point),
    /// Scroll the [`Editor`] a certain amount of lines.
    Scroll {
        /// The amount of lines to scroll.
        lines: i32,
    },
}

impl Action {
    /// Returns whether the [`Action`] is an editing action.
    pub fn is_edit(&self) -> bool {
        matches!(self, Self::Edit(_))
    }
}

/// An action that edits text.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Insert the given character.
    Insert(char),
    /// Paste the given text.
    Paste(Arc<String>),
    /// Break the current line.
    Enter,
    /// Indent the current line.
    Indent,
    /// Unindent the current line.
    Unindent,
    /// Delete the previous character.
    Backspace,
    /// Delete the next character.
    Delete,
}

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    /// Move left.
    Left,
    /// Move right.
    Right,
    /// Move up.
    Up,
    /// Move down.
    Down,
    /// Move to the left boundary of a word.
    WordLeft,
    /// Move to the right boundary of a word.
    WordRight,
    /// Move to the start of the line.
    Home,
    /// Move to the end of the line.
    End,
    /// Move to the start of the previous window.
    PageUp,
    /// Move to the start of the next window.
    PageDown,
    /// Move to the start of the text.
    DocumentStart,
    /// Move to the end of the text.
    DocumentEnd,
}

impl Motion {
    /// Widens the [`Motion`], if possible.
    pub fn widen(self) -> Self {
        match self {
            Self::Left => Self::WordLeft,
            Self::Right => Self::WordRight,
            Self::Home => Self::DocumentStart,
            Self::End => Self::DocumentEnd,
            _ => self,
        }
    }

    /// Returns the [`Direction`] of the [`Motion`].
    pub fn direction(&self) -> Direction {
        match self {
            Self::Left
            | Self::Up
            | Self::WordLeft
            | Self::Home
            | Self::PageUp
            | Self::DocumentStart => Direction::Left,
            Self::Right
            | Self::Down
            | Self::WordRight
            | Self::End
            | Self::PageDown
            | Self::DocumentEnd => Direction::Right,
        }
    }
}

/// A direction in some text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// <-
    Left,
    /// ->
    Right,
}

/// The cursor of an [`Editor`].
#[derive(Debug, Clone)]
pub enum Selection {
    /// Cursor without a selection
    Caret(Point),

    /// Cursor selecting a range of text
    Range(Vec<Rectangle>),
}

/// The range of an [`Editor`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    /// The cursor position.
    pub position: Position,

    /// The selection position, if any.
    pub selection: Option<Position>,
}

/// The vertical scroll position of an [`Editor`].
///
/// It names a LINE and an offset into it, never an absolute pixel distance from
/// the top, so it stays meaningful when the lines above it change height -- or,
/// for an editor whose contents are replaced wholesale, when they are replaced
/// by different lines entirely.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Scroll {
    /// The first line of text in view.
    pub line: usize,
    /// The vertical offset, in pixels, into that line's own visual lines.
    pub offset: f32,
}

/// A cursor position in an [`Editor`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// The line of text.
    pub line: usize,
    /// The column in the line.
    pub column: usize,
}

/// A line of an [`Editor`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Line<'a> {
    /// The raw text of the [`Line`].
    pub text: Cow<'a, str>,
    /// The line ending of the [`Line`].
    pub ending: LineEnding,
}

/// The line ending of a [`Line`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LineEnding {
    /// Use `\n` for line ending (POSIX-style)
    #[default]
    Lf,
    /// Use `\r\n` for line ending (Windows-style)
    CrLf,
    /// Use `\r` for line ending (many legacy systems)
    Cr,
    /// Use `\n\r` for line ending (some legacy systems)
    LfCr,
    /// No line ending
    None,
}

impl LineEnding {
    /// Gets the string representation of the [`LineEnding`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
            Self::LfCr => "\n\r",
            Self::None => "",
        }
    }
}
