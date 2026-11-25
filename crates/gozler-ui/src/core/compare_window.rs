//! The compare window's decisions (DESIGN.md: Compare by content) that need
//! no window: its commands, keys and menus, its title and its status texts,
//! and in [`session`] its state.

pub(crate) mod session;

use super::commands::{Key, Keystroke, Mark, MenuNode};
use super::compare::rtf::Style;
use super::compare::{LINE_COUNT_LIMIT, LINES_LIMIT, SHOWN_LIMIT, Shown};
use super::glyph::Glyph;
use super::layout::{Dpi, Px, Rect, Size};
use super::panel::Side;
use super::text::{self, Numbers, Strings, menu_label};
use super::viewer::{CODE_PAGES, CodePage, Encoding, Mode};
pub(crate) use super::viewer_window::{DEFAULT_ZOOM, zoomed};

/// A command of the compare window's menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompareCommand {
    Compare,
    Save,
    Close,
    Undo,
    Copy,
    Paste,
    SelectAll,
    Find,
    FindNext,
    FindPrevious,
    EditMode,
    CopyToRight,
    CopyToLeft,
    Resync,
    NextDifference,
    PreviousDifference,
    NextInLine,
    PreviousInLine,
    Text,
    Hex,
    OnlyDifferences,
    TwoLineList,
    AboveEachOther,
    Swap,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// Reads one side in another encoding.
    Encoding(Side, Encoding),
}

impl CompareCommand {
    /// The commands besides the encodings, which follow them.
    const FIXED: [CompareCommand; 27] = {
        use CompareCommand as C;
        [
            C::Compare,
            C::Save,
            C::Close,
            C::Undo,
            C::Copy,
            C::Paste,
            C::SelectAll,
            C::Find,
            C::FindNext,
            C::FindPrevious,
            C::EditMode,
            C::CopyToRight,
            C::CopyToLeft,
            C::Resync,
            C::NextDifference,
            C::PreviousDifference,
            C::NextInLine,
            C::PreviousInLine,
            C::Text,
            C::Hex,
            C::OnlyDifferences,
            C::TwoLineList,
            C::AboveEachOther,
            C::Swap,
            C::ZoomIn,
            C::ZoomOut,
            C::ZoomReset,
        ]
    };

    /// The first `WM_COMMAND` ID, above `IDOK` and `IDCANCEL`, which `Enter`
    /// and `Esc` send through `IsDialogMessageW`.
    const FIRST_ID: u16 = 0x100;

    fn all() -> impl Iterator<Item = CompareCommand> {
        Self::FIXED.into_iter().chain(
            Side::BOTH.into_iter().flat_map(|side| {
                Encoding::all().map(move |each| CompareCommand::Encoding(side, each))
            }),
        )
    }

    /// Its `WM_COMMAND` ID in the compare window.
    pub(crate) fn id(self) -> u16 {
        let index = Self::all()
            .position(|command| command == self)
            .expect("all holds every command");
        Self::FIRST_ID + u16::try_from(index).expect("the list is short")
    }

    pub(crate) fn from_id(id: u16) -> Option<CompareCommand> {
        let index = id.checked_sub(Self::FIRST_ID)?;
        Self::all().nth(usize::from(index))
    }

    fn label(self, strings: &Strings) -> String {
        use CompareCommand as C;
        let key = match self {
            C::Compare => "compare.compare",
            C::Save => "compare.save",
            C::Close => "viewer.close",
            C::Undo => "compare.undo",
            C::Copy => "command.copy",
            C::Paste => "command.paste",
            C::SelectAll => "command.select_all",
            C::Find => "viewer.find",
            C::FindNext => "viewer.find_next",
            C::FindPrevious => "viewer.find_previous",
            C::EditMode => "compare.edit_mode",
            C::CopyToRight => "compare.copy_to_right",
            C::CopyToLeft => "compare.copy_to_left",
            C::Resync => "compare.resync",
            C::NextDifference => "compare.next_difference",
            C::PreviousDifference => "compare.previous_difference",
            C::NextInLine => "compare.next_in_line",
            C::PreviousInLine => "compare.previous_in_line",
            C::Text => "viewer.text",
            C::Hex => "viewer.hex",
            C::OnlyDifferences => "compare.only_differences",
            C::TwoLineList => "compare.two_line_list",
            C::AboveEachOther => "compare.stacked",
            C::Swap => "compare.swap",
            C::ZoomIn => "viewer.zoom_in",
            C::ZoomOut => "viewer.zoom_out",
            C::ZoomReset => "viewer.zoom_reset",
            C::Encoding(_, encoding) => return encoding_label(strings, encoding),
        };
        strings.text(key).to_owned()
    }

    /// The key the menu shows: the first of [`KEYS`] that runs it, and
    /// `Esc` for Close, which cancels a comparison that runs first.
    fn keystroke(self) -> Option<Keystroke> {
        if self == CompareCommand::Close {
            return Some(Keystroke::plain(Key::Escape));
        }
        KEYS.iter()
            .find(|(_, key)| *key == CompareKey::Command(self))
            .map(|(keystroke, _)| *keystroke)
    }

    fn mark(self) -> Mark {
        use CompareCommand as C;
        match self {
            C::Text | C::Hex | C::Encoding(..) => Mark::Radio,
            C::EditMode | C::OnlyDifferences | C::TwoLineList | C::AboveEachOther => Mark::Check,
            _ => Mark::None,
        }
    }

    /// The icon, as the main window's command of the same name has.
    fn glyph(self) -> Option<Glyph> {
        use CompareCommand as C;
        match self {
            C::Close => Some(Glyph::Cancel),
            C::Copy => Some(Glyph::Copy),
            C::Paste => Some(Glyph::Paste),
            C::SelectAll => Some(Glyph::SelectAll),
            C::Find => Some(Glyph::Search),
            C::Compare => Some(Glyph::Refresh),
            _ => None,
        }
    }

    /// The tooltip of its button: its menu label and its key, such as "Next
    /// difference (Alt+Down)".
    pub(crate) fn tip(self, strings: &Strings) -> String {
        let label = text::without_access_key(&self.label(strings));
        match self.keystroke() {
            Some(keystroke) => {
                strings.format("compare.command_tip", &[&label, &keystroke.text(strings)])
            }
            None => label,
        }
    }

    fn item(self, strings: &Strings) -> MenuNode {
        MenuNode::Item {
            id: self.id(),
            text: menu_label(
                &self.label(strings),
                self.keystroke().map(|keystroke| keystroke.text(strings)),
            ),
            mark: self.mark(),
            glyph: self.glyph(),
        }
    }
}

/// An encoding's label, as the Viewer's Encoding menu names it.
fn encoding_label(strings: &Strings, encoding: Encoding) -> String {
    let key = match encoding {
        Encoding::CodePage(CodePage::Ansi) => "viewer.ansi",
        Encoding::CodePage(CodePage::Oem) => "viewer.oem",
        Encoding::Utf8 => "viewer.utf8",
        Encoding::Utf16Le => "viewer.utf16_le",
        Encoding::Utf16Be => "viewer.utf16_be",
        Encoding::CodePage(CodePage::Number(number)) => {
            let (_, name, script) = CODE_PAGES
                .iter()
                .find(|(each, ..)| *each == number)
                .expect("the menu offers only the listed code pages");
            return strings.format("viewer.code_page", &[strings.text(script), name]);
        }
    };
    strings.text(key).to_owned()
}

/// The menu of the encodings button: Left ▸ and Right ▸, each with the
/// Viewer's encodings (DESIGN.md: Compare by content).
pub(crate) fn encoding_menu(strings: &Strings) -> Vec<MenuNode> {
    use CompareCommand as C;
    let menu = |label: &str, children| MenuNode::Submenu {
        text: strings.text(label).to_owned(),
        children,
        aligned_right: false,
    };
    let encodings = |side: Side| {
        let item = |encoding| C::Encoding(side, encoding).item(strings);
        let code_pages = CODE_PAGES
            .iter()
            .map(|(number, ..)| item(Encoding::CodePage(CodePage::Number(*number))))
            .collect();
        vec![
            item(Encoding::CodePage(CodePage::Ansi)),
            item(Encoding::CodePage(CodePage::Oem)),
            item(Encoding::Utf8),
            item(Encoding::Utf16Le),
            item(Encoding::Utf16Be),
            MenuNode::Separator,
            menu("viewer.other_code_pages", code_pages),
        ]
    };
    vec![
        menu("compare.left", encodings(Side::Left)),
        menu("compare.right", encodings(Side::Right)),
    ]
}

/// The context menu of a side: Undo | Copy, Paste, Select all | Copy to
/// right, Copy to left | Resync from here | Swap sides, Two-line list. The
/// window has no menu bar, as Total Commander's has none, so the commands
/// that no button shows are here or only on their keys.
pub(crate) fn context_menu(strings: &Strings) -> Vec<MenuNode> {
    use CompareCommand as C;
    vec![
        C::Undo.item(strings),
        MenuNode::Separator,
        C::Copy.item(strings),
        C::Paste.item(strings),
        C::SelectAll.item(strings),
        MenuNode::Separator,
        C::CopyToRight.item(strings),
        C::CopyToLeft.item(strings),
        MenuNode::Separator,
        C::Resync.item(strings),
        MenuNode::Separator,
        C::Swap.item(strings),
        C::TwoLineList.item(strings),
    ]
}

/// What the menus and the buttons show of the window's state.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MenuState {
    /// The path boxes name two files.
    pub(crate) can_compare: bool,
    /// The mode of the comparison that shows, with each side's encoding
    /// in Text; `None` while none shows.
    pub(crate) shown: Option<(Mode, [Option<Encoding>; 2])>,
    /// A difference follows, and one comes before, the caret's row.
    pub(crate) next: bool,
    pub(crate) previous: bool,
    /// Why the comparison that shows is in Hex; `None` when lines can be
    /// compared: both files are text within the limits.
    pub(crate) note: Option<Note>,
    pub(crate) editing: bool,
    pub(crate) can_edit: bool,
    pub(crate) changed: bool,
    pub(crate) can_undo: bool,
    /// Text is selected in the control with the focus.
    pub(crate) selected: bool,
    /// The side with the focus, which Paste and the copies act from.
    pub(crate) side: Option<Side>,
    /// Which sides edit mode can change.
    pub(crate) editable: [bool; 2],
    pub(crate) only_differences: bool,
    pub(crate) two_line_list: bool,
    pub(crate) above_each_other: bool,
    pub(crate) zoom: u16,
}

/// Each menu item's ID, whether it is enabled and whether it is checked.
pub(crate) fn menu_states(state: MenuState) -> Vec<(u16, bool, bool)> {
    CompareCommand::all()
        .map(|command| {
            let (enabled, checked) = command_state(state, command);
            (command.id(), enabled, checked)
        })
        .collect()
}

/// Whether `command` is enabled and whether it is checked, in its menu item
/// and on its button. The encodings are disabled for files too large for
/// Text, and while a side has changes, because reading a file again would
/// lose them; Paste, the copies and Undo outside edit mode, and Paste and a
/// copy into a side that edit mode cannot change.
pub(crate) fn command_state(state: MenuState, command: CompareCommand) -> (bool, bool) {
    use CompareCommand as C;
    let changes = |side: Side| state.editable[side.index()];
    let shown = state.shown.is_some();
    let mode = state.shown.map(|(mode, _)| mode);
    let editing = state.editing && shown;
    match command {
        C::Compare => (state.can_compare, false),
        C::Close | C::Swap => (true, false),
        C::Save => (state.changed, false),
        C::Undo => (editing && state.can_undo, false),
        C::Copy => (state.selected, false),
        C::Paste => (editing && state.side.is_some_and(changes), false),
        C::SelectAll | C::Find | C::FindNext | C::FindPrevious => (shown, false),
        C::EditMode => (state.can_edit, editing),
        C::CopyToRight => (
            editing && state.side.is_some() && changes(Side::Right),
            false,
        ),
        C::CopyToLeft => (
            editing && state.side.is_some() && changes(Side::Left),
            false,
        ),
        C::Resync => (mode == Some(Mode::Text), false),
        C::NextDifference => (shown && state.next, false),
        C::PreviousDifference => (shown && state.previous, false),
        C::NextInLine | C::PreviousInLine => (shown, false),
        C::Text => (shown && state.note.is_none(), mode == Some(Mode::Text)),
        C::Hex => (shown, mode == Some(Mode::Hex)),
        C::OnlyDifferences => (true, state.only_differences),
        C::TwoLineList => (true, state.two_line_list),
        C::AboveEachOther => (true, state.above_each_other),
        C::ZoomIn => (zoomed(state.zoom, 1) != state.zoom, false),
        C::ZoomOut => (zoomed(state.zoom, -1) != state.zoom, false),
        C::ZoomReset => (state.zoom != DEFAULT_ZOOM, false),
        C::Encoding(side, encoding) => {
            let current = state
                .shown
                .and_then(|(_, encodings)| encodings[side.index()]);
            (encodings_enabled(state), current == Some(encoding))
        }
    }
}

/// Whether the encodings can be chosen, which also enables the encodings
/// button. A choice shows Text, also from Hex, as in the Viewer.
pub(crate) fn encodings_enabled(state: MenuState) -> bool {
    let decodable = matches!(state.note, None | Some(Note::NotText));
    state.shown.is_some() && decodable && !state.changed
}

/// A key of the compare window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompareKey {
    /// `Esc` cancels a comparison that runs, and otherwise closes the
    /// window.
    Cancel,
    Command(CompareCommand),
    /// `Insert` or `Space` selects the caret's line, while edit mode is
    /// off.
    SelectLine,
    /// `Tab` while edit mode is off, and `Ctrl+Tab`, move to the next of
    /// the sides and the two-line list; with `Shift` to the previous.
    NextPane {
        forward: bool,
    },
    /// `Shift+F10` opens the context menu.
    ContextMenu,
}

/// The window's keys, Total Commander's (DESIGN.md: Compare by content).
/// The first key of a command is the one its menu item shows.
const KEYS: &[(Keystroke, CompareKey)] = {
    use CompareCommand as C;
    use CompareKey::{Cancel, Command};
    const fn shift(key: Key) -> Keystroke {
        Keystroke {
            shift: true,
            ..Keystroke::plain(key)
        }
    }
    &[
        (Keystroke::plain(Key::Escape), Cancel),
        (Keystroke::ctrl(Key::Letter(b'S')), Command(C::Save)),
        (Keystroke::ctrl(Key::Letter(b'Z')), Command(C::Undo)),
        (Keystroke::alt(Key::Backspace), Command(C::Undo)),
        (Keystroke::ctrl(Key::Letter(b'C')), Command(C::Copy)),
        (Keystroke::ctrl(Key::Insert), Command(C::Copy)),
        (Keystroke::ctrl(Key::Letter(b'V')), Command(C::Paste)),
        (shift(Key::Insert), Command(C::Paste)),
        (Keystroke::ctrl(Key::Letter(b'A')), Command(C::SelectAll)),
        (Keystroke::plain(Key::Function(7)), Command(C::Find)),
        (Keystroke::ctrl(Key::Letter(b'F')), Command(C::Find)),
        (Keystroke::plain(Key::Function(3)), Command(C::FindNext)),
        (shift(Key::Function(3)), Command(C::FindPrevious)),
        (Keystroke::plain(Key::Function(6)), Command(C::EditMode)),
        (Keystroke::alt(Key::Right), Command(C::CopyToRight)),
        (Keystroke::alt(Key::Left), Command(C::CopyToLeft)),
        (Keystroke::alt(Key::Down), Command(C::NextDifference)),
        (Keystroke::alt(Key::Up), Command(C::PreviousDifference)),
        (Keystroke::plain(Key::Function(2)), Command(C::NextInLine)),
        (shift(Key::Function(2)), Command(C::PreviousInLine)),
        (Keystroke::plain(Key::Function(9)), Command(C::TwoLineList)),
        (Keystroke::ctrl(Key::Letter(b'U')), Command(C::Swap)),
        (Keystroke::ctrl(Key::Plus), Command(C::ZoomIn)),
        (Keystroke::ctrl(Key::NumPlus), Command(C::ZoomIn)),
        (Keystroke::ctrl(Key::Minus), Command(C::ZoomOut)),
        (Keystroke::ctrl(Key::NumMinus), Command(C::ZoomOut)),
        (Keystroke::ctrl(Key::Digit(b'0')), Command(C::ZoomReset)),
        (Keystroke::plain(Key::Insert), CompareKey::SelectLine),
        (Keystroke::plain(Key::Space), CompareKey::SelectLine),
        (
            Keystroke::plain(Key::Tab),
            CompareKey::NextPane { forward: true },
        ),
        (shift(Key::Tab), CompareKey::NextPane { forward: false }),
        (
            Keystroke::ctrl(Key::Tab),
            CompareKey::NextPane { forward: true },
        ),
        (
            Keystroke::ctrl_shift(Key::Tab),
            CompareKey::NextPane { forward: false },
        ),
        (shift(Key::Function(10)), CompareKey::ContextMenu),
    ]
};

/// Where the focus is, which decides the keys the window takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyFocus {
    /// A side or the two-line list.
    Text { editing: bool },
    /// A path box or the extra lines, whose keys are the box's own except
    /// `Esc`, `Enter`, the function keys, `Alt+Up`, `Alt+Down` and
    /// `Ctrl+S`.
    Box,
    /// A button or a check box.
    Other,
}

/// The window's key that `keystroke` presses with the focus at `focus`.
pub(crate) fn compare_key(keystroke: Keystroke, focus: KeyFocus) -> Option<CompareKey> {
    if focus == KeyFocus::Box && keystroke == Keystroke::plain(Key::Enter) {
        return Some(CompareKey::Command(CompareCommand::Compare));
    }
    let key = KEYS
        .iter()
        .find(|(each, _)| *each == keystroke)
        .map(|(_, key)| *key)?;
    let editing = matches!(focus, KeyFocus::Text { editing: true });
    let typed_in_text = keystroke == Keystroke::plain(Key::Tab)
        || keystroke
            == Keystroke {
                shift: true,
                ..Keystroke::plain(Key::Tab)
            }
        || key == CompareKey::SelectLine;
    match focus {
        KeyFocus::Text { .. } if editing && typed_in_text => None,
        KeyFocus::Text { .. } => Some(key),
        KeyFocus::Box => {
            // Shift+F10 opens the box's own context menu.
            let kept = matches!(keystroke.key, Key::Escape | Key::Function(_))
                || keystroke == Keystroke::alt(Key::Up)
                || keystroke == Keystroke::alt(Key::Down)
                || keystroke == Keystroke::ctrl(Key::Letter(b'S'));
            (kept && key != CompareKey::ContextMenu).then_some(key)
        }
        // Tab and Space belong to the dialog's navigation and the button.
        KeyFocus::Other => {
            (!typed_in_text && !matches!(key, CompareKey::NextPane { .. })).then_some(key)
        }
    }
}

/// The window's title: both files' names, each after `*` while it has
/// unsaved changes, as Notepad marks them, and the command.
pub(crate) fn title(strings: &Strings, names: [&str; 2], changed: [bool; 2]) -> String {
    match names {
        ["", ""] => strings.text("compare.window").to_owned(),
        _ => {
            let [left, right] = [0, 1]
                .map(|side| format!("{}{}", if changed[side] { "*" } else { "" }, names[side]));
            strings.format("compare.title", &[&left, &right])
        }
    }
}

/// Why the rows show as they do, which the status bar says after the
/// differences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    /// A file is larger than lines are compared for.
    LargeBytes,
    /// A file has more lines than are compared.
    ManyLines,
    /// A file is not text, as the Viewer decides.
    NotText,
    /// Only differences show, because all rows would be too many.
    OnlyDifferences,
    /// The rows stop after this many, the most that show.
    Truncated(usize),
    /// Edit mode is off for a file larger than it works for.
    EditLarge,
}

/// The text of `note`.
pub(crate) fn note_text(strings: &Strings, numbers: &dyn Numbers, note: Note) -> String {
    match note {
        Note::LargeBytes => strings.format(
            "compare.large_bytes",
            &[&text::bytes(strings, numbers, LINES_LIMIT)],
        ),
        Note::ManyLines => strings.format(
            "compare.many_lines",
            &[&numbers.integer(LINE_COUNT_LIMIT as u64)],
        ),
        Note::NotText => strings.text("compare.not_text").to_owned(),
        Note::OnlyDifferences => strings.format(
            "compare.only_forced",
            &[&numbers.integer(SHOWN_LIMIT as u64)],
        ),
        Note::Truncated(rows) => {
            strings.format("compare.truncated", &[&numbers.integer(rows as u64)])
        }
        Note::EditLarge => strings.format(
            "compare.edit_large",
            &[&text::bytes(strings, numbers, super::compare::EDIT_LIMIT)],
        ),
    }
}

/// The status bar's count: "Difference 2 of 3" while the caret is in the
/// difference at `index`, else "3 differences found".
pub(crate) fn differences_text(
    strings: &Strings,
    numbers: &dyn Numbers,
    index: Option<usize>,
    count: usize,
) -> String {
    match (index, count) {
        (_, 0) => strings.text("compare.no_differences").to_owned(),
        (Some(index), _) => strings.format(
            "compare.position",
            &[
                &numbers.integer(index as u64 + 1),
                &numbers.integer(count as u64),
            ],
        ),
        (None, _) => strings.count("compare.differences", count as u64, numbers, &[]),
    }
}

/// The caption of the encodings button: each side's encoding, "UTF-8 ↔
/// ANSI", where a side compared as Hex says Hex; "Encoding" while no
/// comparison shows.
pub(crate) fn encodings_text(
    strings: &Strings,
    encodings: Option<[Option<Encoding>; 2]>,
) -> String {
    let Some(encodings) = encodings else {
        return text::without_access_key(strings.text("viewer.encoding"));
    };
    let hex = text::without_access_key(strings.text("viewer.hex"));
    let [left, right] = encodings
        .map(|encoding| encoding.map_or(hex.clone(), |encoding| encoding.name().to_owned()));
    strings.format("compare.encodings", &[&left, &right])
}

/// The text of a fold's row: "125 lines".
pub(crate) fn fold_text(strings: &Strings, numbers: &dyn Numbers, hidden: u64) -> String {
    strings.count("compare.fold", hidden, numbers, &[])
}

/// A row of `side`'s line-number column for `shown`: the side's line number
/// right-aligned in `digits` and followed by a colon, as Total Commander
/// writes "12:" before a line, or a blank in Hex, where `digits` is `None`
/// because a hex row shows its offset; in red on the row's grey where the
/// row differs, as a grey column where the side has no line, muted
/// otherwise.
pub(crate) fn number_row(shown: &Shown, side: Side, digits: Option<usize>) -> (String, Style) {
    let line = shown.line(side);
    let text = match (digits, line) {
        (None, _) => " ".to_owned(),
        (Some(digits), Some(line)) => format!(" {:>digits$}:", line + 1),
        (Some(digits), None) => " ".repeat(digits + 2),
    };
    let style = match line {
        None => Style::Gap,
        Some(_) if shown.differs() => Style::Differs,
        Some(_) => Style::Muted,
    };
    (text, style)
}

/// What the compare window's layout measures: the sizes of its controls
/// and their captions in the window's fonts.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CompareSizes {
    /// The height of a path box, a button and a check box.
    pub(crate) line: Px,
    /// The height of a label's text.
    pub(crate) text: Px,
    pub(crate) browse: Px,
    /// The first row's buttons: Compare, Next, Previous, Find… and Find
    /// next.
    pub(crate) buttons: [Px; 5],
    /// The captions of the first row's check boxes: Binary, Case sensitive,
    /// Ignore repeated spaces and Ignore frequent lines.
    pub(crate) options: [Px; 4],
    /// The captions of the second row's check boxes: Edit mode, Show only
    /// differences and Above each other.
    pub(crate) views: [Px; 3],
    /// The caption of Extra lines:.
    pub(crate) extra_label: Px,
    /// The widest text of the extra lines box.
    pub(crate) extra: Px,
    pub(crate) save: Px,
    /// The encodings button, at its caption of now.
    pub(crate) encodings: Px,
    pub(crate) cancel: Px,
    /// The height of a row of the sides' text at their zoom.
    pub(crate) row: Px,
    /// The text of each side's line numbers.
    pub(crate) numbers: [Px; 2],
}

/// Where the compare window's controls go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompareLayout {
    pub(crate) paths: [Rect; 2],
    pub(crate) browse: [Rect; 2],
    /// Compare, Next, Previous, Find… and Find next.
    pub(crate) buttons: [Rect; 5],
    /// Binary, Case sensitive, Ignore repeated spaces and Ignore frequent
    /// lines.
    pub(crate) options: [Rect; 4],
    /// Edit mode, Show only differences and Above each other.
    pub(crate) views: [Rect; 3],
    pub(crate) extra_label: Rect,
    pub(crate) extra: Rect,
    pub(crate) save: Rect,
    pub(crate) encodings: Rect,
    pub(crate) progress: Rect,
    pub(crate) cancel: Rect,
    pub(crate) sides: [Rect; 2],
    /// Each side's line numbers, inside the side's border and above its
    /// horizontal scroll bar, so their rows line up with the side's.
    pub(crate) numbers: [Rect; 2],
    /// Why no comparison shows, in the sides' place.
    pub(crate) message: Rect,
    /// The two-line list; no height while it is hidden.
    pub(crate) list: Rect,
}

/// The compare window's layout in `area`, its client above the status
/// bar, in Total Commander's order: the two paths, each with Browse…, in
/// two halves; a row of the buttons that move through the comparison and
/// of the options that decide what matches; a row of edit mode, what shows,
/// Save and the encodings, with the progress and Cancel at its end; the
/// sides beside or `above_each_other`; and the two-line list. A row that
/// is too narrow goes on in a row below, and Show only differences keeps
/// its extra lines beside it. The progress and Cancel keep their place
/// while they are hidden, so the sides do not move when a comparison
/// starts or ends.
pub(crate) fn compare_layout(
    area: Size,
    sizes: &CompareSizes,
    above_each_other: bool,
    two_line_list: bool,
    dpi: Dpi,
) -> CompareLayout {
    let px = |dip: i32| {
        Px(dip)
            .rescale(Dpi::new(96).expect("96 is not zero"), dpi)
            .0
    };
    // The message stands as far below the top of the sides' place as the
    // Viewer's below its area's.
    let (margin, gap, edge, scroll_bar, message_top) = (px(11), px(7), px(2), px(17), px(60));
    let rect = |x: i32, y: i32, width: i32, height: i32| Rect {
        x: Px(x),
        y: Px(y),
        width: Px(width.max(0)),
        height: Px(height.max(0)),
    };
    let width = area.width.0;
    let line = sizes.line.0;
    let text = sizes.text.0;
    let label = |x: i32, y: i32, caption: Px| rect(x, y + (line - text) / 2, caption.0, text);
    let half = ((width - 2 * margin - gap) / 2).max(0);
    let mut layout = CompareLayout::default();
    for (index, x) in [margin, margin + half + gap].into_iter().enumerate() {
        let browse = sizes.browse.0.min(half);
        layout.paths[index] = rect(x, margin, half - browse - gap, line);
        layout.browse[index] = rect(x + half - browse, margin, browse, line);
    }
    // Each item goes on its row while it fits, else at the start of the
    // next; `lead` is the space before it on the same row.
    let end = width - margin;
    let place = |at: &mut (i32, i32), item_width: i32, lead: i32| {
        let (mut x, mut y) = *at;
        if x > margin && x + lead + item_width > end {
            (x, y) = (margin, y + line + gap);
        } else if x > margin {
            x += lead;
        }
        *at = (x + item_width, y);
        rect(x, y, item_width, line)
    };
    let mut at = (margin, margin + line + gap);
    let check = |caption: Px| caption.0 + px(20);
    for (index, button) in sizes.buttons.into_iter().enumerate() {
        // Find… and Find next are a group of their own.
        let lead = if index == 3 { 2 * gap } else { gap };
        layout.buttons[index] = place(&mut at, button.0, lead);
    }
    for (index, caption) in sizes.options.into_iter().enumerate() {
        let lead = if index == 0 { 2 * gap } else { gap };
        layout.options[index] = place(&mut at, check(caption), lead);
    }
    // The second row starts below the first, however the first went on.
    at = (margin, at.1 + line + gap);
    layout.views[0] = place(&mut at, check(sizes.views[0]), 0);
    let extra_box = sizes.extra.0 + px(16);
    let only = check(sizes.views[1]) + gap + sizes.extra_label.0 + px(4) + extra_box;
    let group = place(&mut at, only, 2 * gap);
    layout.views[1] = rect(group.x.0, group.y.0, check(sizes.views[1]), line);
    layout.extra_label = label(
        layout.views[1].right().0 + gap,
        group.y.0,
        sizes.extra_label,
    );
    layout.extra = rect(group.right().0 - extra_box, group.y.0, extra_box, line);
    layout.views[2] = place(&mut at, check(sizes.views[2]), 2 * gap);
    layout.save = place(&mut at, sizes.save.0, 2 * gap);
    layout.encodings = place(&mut at, sizes.encodings.0, gap);
    let cancel = sizes.cancel.0;
    let progress = px(100);
    let running = place(&mut at, progress + gap + cancel, 2 * gap);
    layout.cancel = rect(end - cancel, running.y.0, cancel, line);
    layout.progress = rect(
        layout.cancel.x.0 - gap - progress,
        running.y.0 + (line - px(12)) / 2,
        progress,
        px(12),
    );
    let top = running.y.0 + line + gap;
    let list_height = if two_line_list {
        2 * sizes.row.0 + 2 * edge + px(4)
    } else {
        0
    };
    let list_gap = if two_line_list { gap } else { 0 };
    let bottom = (area.height.0 - list_height - list_gap).max(top);
    let sides = if above_each_other {
        let height = ((bottom - top - gap) / 2).max(0);
        [
            rect(0, top, width, height),
            rect(0, top + height + gap, width, height),
        ]
    } else {
        let side = ((width - gap) / 2).max(0);
        [
            rect(0, top, side, bottom - top),
            rect(side + gap, top, width - side - gap, bottom - top),
        ]
    };
    for (index, side) in sides.into_iter().enumerate() {
        // The line numbers stand inside the side's left edge, and the
        // side's text starts after them.
        let numbers = (sizes.numbers[index].0 + px(4)).min(side.width.0 / 2);
        layout.numbers[index] = rect(
            side.x.0 + edge,
            side.y.0 + edge,
            numbers,
            side.height.0 - 2 * edge - scroll_bar,
        );
        layout.sides[index] = rect(
            side.x.0 + edge + numbers,
            side.y.0,
            side.width.0 - edge - numbers,
            side.height.0,
        );
    }
    layout.message = rect(
        margin,
        top + message_top,
        width - 2 * margin,
        bottom - top - message_top,
    );
    layout.list = rect(0, bottom + list_gap, width, list_height);
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes() -> CompareSizes {
        CompareSizes {
            line: Px(23),
            text: Px(15),
            browse: Px(75),
            buttons: [Px(75), Px(75), Px(75), Px(60), Px(75)],
            options: [Px(45), Px(80), Px(120), Px(110)],
            views: [Px(55), Px(130), Px(95)],
            extra_label: Px(60),
            extra: Px(20),
            save: Px(60),
            encodings: Px(110),
            cancel: Px(75),
            row: Px(15),
            numbers: [Px(30), Px(40)],
        }
    }

    /// The sides share the width, or the height above each other, and the
    /// line numbers stand left of each side's text, above its scroll bar.
    #[test]
    fn the_sides_split_the_window_beside_or_above_each_other() {
        let dpi = Dpi::new(96).expect("96 is not zero");
        let area = Size {
            width: Px(1400),
            height: Px(700),
        };
        let beside = compare_layout(area, &sizes(), false, true, dpi);
        assert_eq!(beside.sides[0].y, beside.sides[1].y);
        assert!(beside.sides[0].right() <= beside.numbers[1].x);
        assert_eq!(beside.numbers[0].right(), beside.sides[0].x);
        assert!(beside.numbers[0].bottom() < beside.sides[0].bottom());
        assert_eq!(beside.list.height, Px(2 * 15 + 2 * 2 + 4));
        assert!(beside.list.y > beside.sides[0].bottom());
        let above = compare_layout(area, &sizes(), true, false, dpi);
        assert!(above.sides[0].bottom() < above.sides[1].y);
        assert_eq!(above.sides[1].bottom(), Px(700));
        assert_eq!(above.list.height, Px(0));
    }

    /// A wide window holds the buttons and the options in one row and edit
    /// mode, what shows, Save, the encodings and Cancel in the next; a
    /// narrow one goes on in rows below, with nothing past its edge, Show
    /// only differences beside its extra lines, and the sides below Cancel,
    /// which keeps its place while it is hidden.
    #[test]
    fn the_rows_go_on_below_in_a_narrow_window() {
        let dpi = Dpi::new(96).expect("96 is not zero");
        let at = |width: i32| {
            let area = Size {
                width: Px(width),
                height: Px(700),
            };
            compare_layout(area, &sizes(), false, false, dpi)
        };
        let wide = at(1400);
        assert_eq!(wide.buttons[0].y, wide.options[3].y);
        assert!(wide.views[0].y > wide.buttons[0].y);
        assert_eq!(wide.views[0].x, wide.buttons[0].x);
        assert_eq!(wide.views[0].y, wide.cancel.y);
        assert_eq!(wide.views[0].y, wide.encodings.y);
        let narrow = at(500);
        assert!(narrow.cancel.y > narrow.views[0].y);
        assert_eq!(narrow.extra.y, narrow.views[1].y);
        assert!(narrow.extra_label.right() <= narrow.extra.x);
        let all = [
            narrow.buttons.as_slice(),
            narrow.options.as_slice(),
            narrow.views.as_slice(),
        ]
        .concat();
        for each in all.iter().chain([
            &narrow.extra,
            &narrow.save,
            &narrow.encodings,
            &narrow.cancel,
        ]) {
            assert!(each.right() <= Px(500 - 11), "{each:?}");
        }
        assert!(narrow.views[0].y > narrow.options[3].y);
        assert!(narrow.sides[0].y > narrow.cancel.bottom());
    }

    /// Each key has one meaning, each command's ID reads back, and the
    /// keys that type in a side stay its own in edit mode and a path box's
    /// own there.
    #[test]
    fn keys_follow_the_focus() {
        for (index, (keystroke, _)) in KEYS.iter().enumerate() {
            assert!(
                KEYS[..index].iter().all(|(each, _)| each != keystroke),
                "{keystroke:?} has two meanings"
            );
        }
        for command in CompareCommand::all() {
            assert_eq!(CompareCommand::from_id(command.id()), Some(command));
        }
        let space = Keystroke::plain(Key::Space);
        let read = KeyFocus::Text { editing: false };
        let edit = KeyFocus::Text { editing: true };
        assert_eq!(compare_key(space, read), Some(CompareKey::SelectLine));
        assert_eq!(compare_key(space, edit), None);
        assert_eq!(compare_key(Keystroke::plain(Key::Tab), edit), None);
        let copy = Keystroke::ctrl(Key::Letter(b'C'));
        assert_eq!(compare_key(copy, KeyFocus::Box), None);
        assert_eq!(
            compare_key(Keystroke::plain(Key::Enter), KeyFocus::Box),
            Some(CompareKey::Command(CompareCommand::Compare))
        );
        assert_eq!(
            compare_key(Keystroke::plain(Key::Function(7)), KeyFocus::Box),
            Some(CompareKey::Command(CompareCommand::Find))
        );
    }
}
