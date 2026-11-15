//! Column widths: how a column fits its header and cells, how the fits
//! fill the list's width, and when a list's columns fill (DESIGN.md:
//! Results list, A panel). The rules are TableView's, so a column's width
//! comes only from what it shows. The window measures the texts and sets
//! the widths that [`ColumnSizing`] and [`fill_widths`] answer with.

use super::{Dip, Dpi, Px, cell_text_inset};
use crate::core::model::Column;

/// The space a header leaves on each side of its text.
const HEADER_TEXT_MARGIN: Dip = Dip(6);

/// The width a fill leaves a column with content when its fit is wider,
/// TableView's default column width, so a narrow list does not cut a name
/// to its first letter.
const KEPT_WIDTH: Dip = Dip(150);

/// How long a new search's results, or a cut value, show before the
/// columns fill, so the results a person passes through while typing, and
/// a burst of updates, fill once.
pub(crate) const FILL_DELAY_MS: u32 = 300;

/// What a column's header and the cells a fit measured ask of a fill
/// (DESIGN.md: Results list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColumnFit {
    pub(crate) column: Column,
    /// The width that shows the header and every measured cell whole.
    pub(crate) widest: Px,
    /// No measured cell shows anything, so `widest` is the header's.
    pub(crate) empty: bool,
    /// The narrowest a fill makes the column: `widest` when it cannot
    /// shrink, nothing when it is empty, and otherwise [`KEPT_WIDTH`], or
    /// `widest` when that is narrower.
    pub(crate) least: Px,
    /// Whether a fill may make the column wider than `widest`.
    pub(crate) grows: bool,
}

impl ColumnFit {
    /// The fit of `column`, whose header text is `header` wide and whose
    /// measured cells that show anything have texts `texts` wide, in the
    /// list's first column when `first`, with each text starting `label`
    /// after the column's left edge.
    pub(crate) fn of(
        column: Column,
        header: Px,
        texts: impl IntoIterator<Item = Px>,
        label: Px,
        first: bool,
        dpi: Dpi,
    ) -> ColumnFit {
        let margin = HEADER_TEXT_MARGIN.to_px(dpi);
        let mut widest = header + margin + margin;
        let mut empty = true;
        for text in texts {
            widest = widest.max(cell_width(text, label, first, dpi));
            empty = false;
        }
        let row = column.row();
        let least = if !row.can_shrink {
            widest
        } else if empty {
            Px(0)
        } else {
            widest.min(KEPT_WIDTH.to_px(dpi))
        };
        ColumnFit {
            column,
            widest,
            empty,
            least,
            grows: row.can_grow,
        }
    }
}

/// The width of a cell whose text is `text` wide, in the column `first`
/// when it is the list's first, whose text starts `label` after the
/// column's left edge, past the row's icon: the text with the list's
/// space on each side.
pub(crate) fn cell_width(text: Px, label: Px, first: bool, dpi: Dpi) -> Px {
    let inset = cell_text_inset(first, dpi);
    label + inset + text + inset
}

/// The widths that make columns with these `fits` end at `width`, as
/// TableView's fill does (DESIGN.md: Results list). Spare width goes to
/// the columns with content, because an empty column has nothing to show
/// in more. A shortfall comes from the empty columns first, because a cut
/// header hides less than a cut value. When no column is empty, or every
/// column is, they all take part at once. A column that cannot grow keeps
/// its fit, so when no column that takes part can grow, the columns end
/// before the edge; when even their least widths do not fit, they run
/// past it.
pub(crate) fn fill_widths(fits: &[ColumnFit], width: Px) -> Vec<Px> {
    let mut widths: Vec<f64> = fits.iter().map(|fit| f64::from(fit.widest.0)).collect();
    let mut change = f64::from(width.0) - widths.iter().sum::<f64>();
    let all: Vec<usize> = (0..fits.len()).collect();
    let empty: Vec<usize> = all.iter().copied().filter(|&i| fits[i].empty).collect();
    let content: Vec<usize> = all.iter().copied().filter(|&i| !fits[i].empty).collect();
    let steps = if empty.is_empty() || content.is_empty() {
        vec![all]
    } else if change > 0.0 {
        vec![content]
    } else {
        vec![empty, content]
    };
    for step in steps {
        change = scale(&mut widths, fits, &step, change);
    }
    whole_pixels(&widths)
}

/// Changes the widths of the columns at `step` by `change` in all, by one
/// factor, so the widest columns keep the most room and a resize makes no
/// column jump. A column that the factor would take below its least width,
/// or past its fit when it cannot grow, stays at that bound, and the
/// others share what is left. Answers the part of `change` that the
/// columns could not take.
fn scale(widths: &mut [f64], fits: &[ColumnFit], step: &[usize], change: f64) -> f64 {
    let bounds = |i: usize| {
        let fit = f64::from(fits[i].widest.0);
        let least = f64::from(fits[i].least.0);
        let most = if fits[i].grows { f64::INFINITY } else { fit };
        (least, most)
    };
    let mut free: Vec<usize> = step.to_vec();
    let mut left = change;
    while !free.is_empty() && left != 0.0 {
        let weight: f64 = free.iter().map(|&i| widths[i]).sum();
        if weight <= 0.0 {
            break;
        }
        let factor = 1.0 + left / weight;
        let held: Vec<usize> = (free.iter().copied())
            .filter(|&i| {
                let (least, most) = bounds(i);
                !(least..=most).contains(&(widths[i] * factor))
            })
            .collect();
        if held.is_empty() {
            for &i in &free {
                widths[i] *= factor;
            }
            return 0.0;
        }
        for &i in &held {
            let (least, most) = bounds(i);
            let bound = (widths[i] * factor).clamp(least, most);
            left -= bound - widths[i];
            widths[i] = bound;
        }
        free.retain(|i| !held.contains(i));
    }
    left
}

/// `widths` in whole pixels that add up to their rounded sum, so the
/// columns end exactly at the edge whenever they reach it: each rounded
/// down, and the pixels that leaves going one each to the columns that
/// lost the most.
fn whole_pixels(widths: &[f64]) -> Vec<Px> {
    // Rounded down, so the sum is no wider than the widths it rounds.
    let mut pixels: Vec<Px> = widths.iter().map(|&w| Px(w as i32)).collect();
    let total = widths.iter().sum::<f64>().round() as i32;
    let left = total - pixels.iter().map(|px| px.0).sum::<i32>();
    let mut by_loss: Vec<usize> = (0..widths.len()).collect();
    by_loss.sort_by(|&a, &b| {
        (widths[b] - widths[b].floor()).total_cmp(&(widths[a] - widths[a].floor()))
    });
    for &i in by_loss.iter().take(usize::try_from(left).unwrap_or(0)) {
        pixels[i].0 += 1;
    }
    pixels
}

/// Rows that replace those a list shows, which decide when its columns
/// fill (DESIGN.md: Results list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NewRows {
    /// A different list, such as a new search's results or another folder
    /// in a panel: the columns fill once the list has shown it for
    /// [`FILL_DELAY_MS`], so the lists a person passes through while typing
    /// or moving between folders show at once and do not move the columns.
    Different,
    /// A sort's first order: the columns fill as the rows show, so the
    /// person sees one change.
    Order,
}

/// What [`ColumnSizing::rows_shown`] asks of the columns.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fill {
    Keep,
    /// The columns take these widths now.
    Widths(Vec<Px>),
    /// The wait of [`FILL_DELAY_MS`] starts again from now, and
    /// [`ColumnSizing::waited`] follows it.
    Wait,
}

/// What the columns do when the wait of [`FILL_DELAY_MS`] ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterWait {
    /// A different list showed: the columns fill.
    Fill,
    /// Rows arrived, scrolled into view or changed their values: the
    /// columns fill when a value is cut off.
    FillIfCut,
}

/// When one list's columns fill its width, and the widths that change
/// between fills (DESIGN.md: Results list). Each event that fills the
/// columns measures their fits with the `measure` it is given and answers
/// with the widths.
#[derive(Debug)]
pub(crate) struct ColumnSizing {
    /// Whether the columns fill by themselves: Auto fit in the header's
    /// menu. While it is off, only the person and Fit column change a
    /// width.
    automatic: bool,
    /// The width the columns last filled, so a layout that keeps it fills
    /// nothing.
    filled_width: Option<Px>,
    /// A drag of the window's border or title bar, between
    /// `WM_ENTERSIZEMOVE` and `WM_EXITSIZEMOVE`.
    drag: Option<Drag>,
    /// The width each column had when the person hid it, at the DPI then,
    /// which it gets again when shown, for the window's life.
    hidden: Vec<(Column, Px, Dpi)>,
    /// Each column's fit at the last fill or Fit column, which a cut value
    /// compares the column's width with.
    fits: Vec<ColumnFit>,
    /// Whether a fill has measured rows. Until one has, the columns fit
    /// the headers alone, so the list's first rows fill as they show.
    rows_fitted: bool,
    /// What the rows that show next replace.
    new_rows: Option<NewRows>,
    /// The wait of [`FILL_DELAY_MS`] that runs; a fill before it ends
    /// serves the rows on screen too.
    waiting: Option<AfterWait>,
    /// The row the list last drew from, so a draw from another row tells
    /// that rows scrolled into view.
    top_row: Option<u64>,
}

#[derive(Debug, Default)]
struct Drag {
    /// A fill of the drag measured [`ColumnSizing::fits`], which the
    /// drag's next fills scale, so the widths follow the pointer without
    /// measuring every time.
    measured: bool,
}

impl ColumnSizing {
    /// With Auto fit `automatic`.
    pub(crate) fn new(automatic: bool) -> ColumnSizing {
        ColumnSizing {
            automatic,
            filled_width: None,
            drag: None,
            hidden: Vec::new(),
            fits: Vec::new(),
            rows_fitted: false,
            new_rows: None,
            waiting: None,
            top_row: None,
        }
    }

    /// The list is `width` wide after a layout: when the window shows, is
    /// resized, or a pane beside the list shows, hides or moves. Answers
    /// the widths that fill it, or `None` when the columns filled that
    /// width last. A drag measures once and scales those fits at each
    /// width; [`Self::drag_ended`] measures again at the final width.
    pub(crate) fn resized(
        &mut self,
        width: Px,
        measure: impl FnOnce() -> Vec<ColumnFit>,
    ) -> Option<Vec<Px>> {
        if !self.automatic || self.filled_width == Some(width) {
            return None;
        }
        let fits = match &mut self.drag {
            Some(drag) if drag.measured => self.fits.clone(),
            Some(drag) => {
                drag.measured = true;
                measure()
            }
            None => measure(),
        };
        Some(self.fill_with(width, fits))
    }

    pub(crate) fn drag_started(&mut self) {
        // Fits measured before this drag were measured on other rows.
        self.drag = Some(Drag::default());
    }

    /// The drag ended with the list `width` wide: when the drag resized
    /// the list, the widths that fit the rows it shows now.
    pub(crate) fn drag_ended(
        &mut self,
        width: Px,
        measure: impl FnOnce() -> Vec<ColumnFit>,
    ) -> Option<Vec<Px>> {
        if !self.drag.take()?.measured {
            return None;
        }
        self.fill(width, measure)
    }

    /// The window moved from a monitor of `from` DPI to one of `to`, with
    /// the columns `widths` wide: the widths and fits are in the old DPI's
    /// pixels, so the next layout fills afresh. While Auto fit is off,
    /// the answer is the widths the person kept, at the new DPI.
    pub(crate) fn dpi_changed(&mut self, widths: &[Px], from: Dpi, to: Dpi) -> Option<Vec<Px>> {
        self.forget_drag_fits();
        self.filled_width = None;
        (!self.automatic).then(|| widths.iter().map(|width| width.rescale(from, to)).collect())
    }

    /// The rows that the list shows next replace its rows; the list then
    /// tells [`Self::rows_shown`].
    pub(crate) fn new_rows(&mut self, rows: NewRows) {
        // A new order fills at once, also when a new search brings it.
        if self.new_rows != Some(NewRows::Order) {
            self.new_rows = Some(rows);
        }
    }

    /// The list, whose columns fill `width`, shows rows that arrived or
    /// changed their values, or those [`Self::new_rows`] announced; rows
    /// that scroll into view tell [`Self::drawn_from`]. A sort's first
    /// order, and the list's first rows, fill at once. Everything else
    /// waits for [`FILL_DELAY_MS`] and measures nothing now, because each
    /// measure formats and measures every cell on screen, so a person who
    /// moves through lists or folders, and a burst of updates, never wait
    /// for one ([`Self::waited`]).
    pub(crate) fn rows_shown(
        &mut self,
        width: Px,
        measure: impl FnOnce() -> Vec<ColumnFit>,
    ) -> Fill {
        let new_rows = self.new_rows.take();
        if !self.automatic {
            return Fill::Keep;
        }
        if new_rows == Some(NewRows::Order) {
            return Fill::Widths(self.fill_with(width, measure()));
        }
        if !self.rows_fitted {
            let fits = measure();
            return if fits.iter().all(|fit| fit.empty) {
                Fill::Keep
            } else {
                Fill::Widths(self.fill_with(width, fits))
            };
        }
        match (new_rows, self.waiting) {
            // Each different list starts the wait again.
            (Some(NewRows::Different), _) => {
                self.waiting = Some(AfterWait::Fill);
                Fill::Wait
            }
            // The wait that runs checks these rows too, so a burst of
            // updates measures once.
            (_, Some(_)) => Fill::Keep,
            _ => {
                self.waiting = Some(AfterWait::FillIfCut);
                Fill::Wait
            }
        }
    }

    /// The list draws its rows from row `top` on. A `top` other than at
    /// the last draw means that a key, the wheel, the scroll bar or a jump
    /// brought other rows into view, and then the answer is whether the
    /// wait of [`FILL_DELAY_MS`] starts again, so a person who scrolls
    /// costs one measure, after they stop. It measures nothing, because
    /// the list is drawing.
    pub(crate) fn drawn_from(&mut self, top: u64) -> bool {
        if self.top_row.replace(top) == Some(top) || !self.automatic || !self.rows_fitted {
            return false;
        }
        self.waiting.get_or_insert(AfterWait::FillIfCut);
        true
    }

    /// The wait that [`Fill::Wait`] asked for ended, with the columns
    /// `current` wide: the widths that fill the list, unless a fill came
    /// first. A list that shows nothing, such as an empty folder or a
    /// panel that shows reading, keeps the widths, because the next rows
    /// would otherwise show in widths that fit the headers alone. After
    /// rows that changed, the columns fill only when a value is cut off in
    /// a column at least as wide as its last fit, because a column that a
    /// fill or the person made narrower would only be narrowed again or
    /// lose the person's width.
    pub(crate) fn waited(
        &mut self,
        current: &[Px],
        width: Px,
        measure: impl FnOnce() -> Vec<ColumnFit>,
    ) -> Option<Vec<Px>> {
        let after = self.waiting.take()?;
        if !self.automatic {
            return None;
        }
        let fits = measure();
        if fits.iter().all(|fit| fit.empty) {
            return None;
        }
        let fill = match after {
            AfterWait::Fill => true,
            AfterWait::FillIfCut => fits.iter().zip(current).any(|(fit, &shown)| {
                fit.widest > shown
                    && (self.fits.iter())
                        .any(|last| last.column == fit.column && shown >= last.widest)
            }),
        };
        fill.then(|| self.fill_with(width, fits))
    }

    /// The widths that fill the list now: when the window shows, because a
    /// hidden window shows at the width it had, which no resize fills;
    /// when every Size cell changes its unit; and when a panel's columns
    /// change. `None` while Auto fit is off.
    pub(crate) fn fill(
        &mut self,
        width: Px,
        measure: impl FnOnce() -> Vec<ColumnFit>,
    ) -> Option<Vec<Px>> {
        if !self.automatic {
            return None;
        }
        Some(self.fill_with(width, measure()))
    }

    /// The person switched Auto fit: switched on, the columns fill at
    /// once, because the person who asked for fitted columns looks at
    /// them.
    pub(crate) fn set_automatic(
        &mut self,
        automatic: bool,
        width: Px,
        measure: impl FnOnce() -> Vec<ColumnFit>,
    ) -> Option<Vec<Px>> {
        self.automatic = automatic;
        self.fill(width, measure)
    }

    /// Fit column gave `fit`'s column its width, which a cut value
    /// compares the column's width with from now on.
    pub(crate) fn column_fitted(&mut self, fit: ColumnFit) {
        // In place, because a drag scales the fits in the columns' order.
        match self.fits.iter_mut().find(|last| last.column == fit.column) {
            Some(last) => *last = fit,
            None => self.fits.push(fit),
        }
    }

    /// The person hid `column`, which was `width` wide on a monitor of
    /// `dpi`.
    pub(crate) fn column_hidden(&mut self, column: Column, width: Px, dpi: Dpi) {
        self.forget_drag_fits();
        self.hidden.retain(|(hidden, ..)| *hidden != column);
        self.hidden.push((column, width, dpi));
    }

    /// The width of `column`, shown on a monitor of `dpi`: the width it
    /// had when hidden, as TableView's columns do, or `None` for a column
    /// not hidden before, which then fits its header and rows.
    pub(crate) fn column_shown(&mut self, column: Column, dpi: Dpi) -> Option<Px> {
        self.forget_drag_fits();
        (self.hidden.iter())
            .find(|(hidden, ..)| *hidden == column)
            .map(|(_, width, then)| width.rescale(*then, dpi))
    }

    /// Fills `width` from `fits`, which a drag that follows scales,
    /// because they are the fits of the rows on screen.
    fn fill_with(&mut self, width: Px, fits: Vec<ColumnFit>) -> Vec<Px> {
        if let Some(drag) = &mut self.drag {
            drag.measured = true;
        }
        self.filled_width = Some(width);
        self.waiting = None;
        self.rows_fitted |= fits.iter().any(|fit| !fit.empty);
        let widths = fill_widths(&fits, width);
        self.fits = fits;
        widths
    }

    /// The fits no longer match the columns or the DPI, so the drag's next
    /// fill measures.
    fn forget_drag_fits(&mut self) {
        if let Some(drag) = &mut self.drag {
            drag.measured = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn dpi(dpi: u32) -> Dpi {
        Dpi::new(dpi).expect("not zero")
    }

    /// A Name column with content, whose least width is [`KEPT_WIDTH`] at
    /// 96 DPI, or its fit when narrower.
    fn fit(widest: i32) -> ColumnFit {
        ColumnFit {
            column: Column::Name,
            widest: Px(widest),
            empty: false,
            least: Px(widest.min(150)),
            grows: true,
        }
    }

    /// A column that a fill neither shrinks nor grows, such as Size.
    fn whole(widest: i32) -> ColumnFit {
        ColumnFit {
            column: Column::Size,
            least: Px(widest),
            grows: false,
            ..fit(widest)
        }
    }

    /// A column whose measured rows show nothing, so its fit is its header.
    fn empty(header: i32) -> ColumnFit {
        ColumnFit {
            column: Column::Path,
            empty: true,
            least: Px(0),
            ..fit(header)
        }
    }

    /// A measure of two columns with content that counts its calls,
    /// because each call measures every shown cell's text.
    fn measure(calls: &Cell<u32>) -> impl Fn() -> Vec<ColumnFit> + '_ {
        || {
            calls.set(calls.get() + 1);
            vec![
                fit(100),
                ColumnFit {
                    column: Column::Path,
                    ..fit(300)
                },
            ]
        }
    }

    /// In a list too narrow for the fits, the columns with content shrink
    /// by one factor, and Size, which cannot shrink, keeps its fit.
    #[test]
    fn a_shortfall_scales_the_columns_that_can_shrink() {
        let fits = [fit(300), fit(900), whole(70)];
        assert_eq!(fill_widths(&fits, Px(670)), [Px(150), Px(450), Px(70)]);
    }

    /// Spare width goes to the columns with content that can grow, by one
    /// factor; an empty column and Size keep their fits.
    #[test]
    fn spare_width_goes_to_the_columns_with_content() {
        let fits = [fit(200), empty(60), whole(70), fit(100)];
        assert_eq!(
            fill_widths(&fits, Px(730)),
            [Px(400), Px(60), Px(70), Px(200)]
        );
    }

    /// A shortfall comes from the empty columns first.
    #[test]
    fn a_shortfall_comes_from_the_empty_columns_first() {
        let fits = [fit(300), empty(100), fit(100)];
        assert_eq!(fill_widths(&fits, Px(450)), [Px(300), Px(50), Px(100)]);
    }

    /// A name keeps 150 DIP, so the rows stay apart, and when even that
    /// does not fit, the columns run past the edge.
    #[test]
    fn a_name_keeps_its_least_width_past_the_edge() {
        let fits = [fit(300), whole(70), whole(120)];
        assert_eq!(fill_widths(&fits, Px(100)), [Px(150), Px(70), Px(120)]);
    }

    /// Each fill measures every shown cell and sets every width, so a
    /// layout that keeps the width, such as a pane's text changing, fills
    /// nothing.
    #[test]
    fn a_layout_at_the_width_filled_last_fills_nothing() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(true);
        assert_eq!(
            sizing.resized(Px(800), measure(&calls)),
            Some(vec![Px(200), Px(600)])
        );
        assert_eq!(sizing.resized(Px(800), measure(&calls)), None);
        assert_eq!(calls.get(), 1);
    }

    /// A drag measures once, so the widths follow the pointer, and once
    /// more at its end, on the rows the list shows then; a drag that only
    /// moved the window fills nothing.
    #[test]
    fn a_drag_measures_once_and_again_at_its_end() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(true);
        sizing.drag_started();
        assert_eq!(sizing.drag_ended(Px(800), measure(&calls)), None);
        sizing.drag_started();
        sizing.resized(Px(800), measure(&calls));
        assert_eq!(
            sizing.resized(Px(400), measure(&calls)),
            Some(vec![Px(100), Px(300)])
        );
        assert_eq!(calls.get(), 1);
        assert!(sizing.drag_ended(Px(400), measure(&calls)).is_some());
        assert_eq!(calls.get(), 2);
    }

    /// The widths are in the old DPI's pixels, so the same width in pixels
    /// on the new monitor fills again; with Auto fit off, the person's
    /// widths keep their size.
    #[test]
    fn a_dpi_change_fills_again_or_scales_the_kept_widths() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(true);
        sizing.resized(Px(800), measure(&calls));
        assert_eq!(sizing.dpi_changed(&[Px(90)], dpi(96), dpi(144)), None);
        assert!(sizing.resized(Px(800), measure(&calls)).is_some());

        let mut kept = ColumnSizing::new(false);
        assert_eq!(
            kept.dpi_changed(&[Px(90)], dpi(96), dpi(144)),
            Some(vec![Px(135)])
        );
    }

    /// Before any fill measured rows, the first rows fill at once, and a
    /// drag that follows scales their fits, not those of the headers
    /// alone.
    #[test]
    fn the_first_rows_fill_once_and_a_drag_scales_their_fits() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(true);
        sizing.drag_started();
        sizing.resized(Px(800), || vec![empty(100)]);
        assert!(matches!(
            sizing.rows_shown(Px(800), measure(&calls)),
            Fill::Widths(_)
        ));
        assert_eq!(sizing.rows_shown(Px(800), measure(&calls)), Fill::Wait);
        assert_eq!(
            sizing.resized(Px(700), measure(&calls)),
            Some(vec![Px(175), Px(525)])
        );
        assert_eq!(calls.get(), 1);
    }

    /// A different list shows without a measure and fills after the wait,
    /// so a person who moves through folders or types never waits for a
    /// fill, and a sort's first order fills at once.
    #[test]
    fn a_different_list_waits_and_a_new_order_fills_at_once() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(true);
        sizing.resized(Px(800), measure(&calls));
        let current = [Px(200), Px(600)];
        sizing.new_rows(NewRows::Different);
        assert_eq!(sizing.rows_shown(Px(800), measure(&calls)), Fill::Wait);
        assert_eq!(calls.get(), 1);
        assert!(sizing.waited(&current, Px(800), measure(&calls)).is_some());
        assert_eq!(sizing.waited(&current, Px(800), measure(&calls)), None);
        sizing.new_rows(NewRows::Order);
        sizing.new_rows(NewRows::Different);
        assert!(matches!(
            sizing.rows_shown(Px(800), measure(&calls)),
            Fill::Widths(_)
        ));
    }

    /// A list that shows nothing, such as an empty folder, keeps the widths
    /// when its wait ends, and the list after it waits as any different
    /// list does, also after a resize while the empty one showed, so
    /// leaving an empty folder never measures in the frame that shows the
    /// next one.
    #[test]
    fn a_list_after_an_empty_one_waits() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(true);
        sizing.resized(Px(800), measure(&calls));
        let nothing = || vec![empty(100), empty(100)];
        sizing.new_rows(NewRows::Different);
        assert_eq!(sizing.rows_shown(Px(800), nothing), Fill::Wait);
        assert_eq!(sizing.waited(&[Px(200), Px(600)], Px(800), nothing), None);
        assert!(sizing.resized(Px(700), nothing).is_some());
        sizing.new_rows(NewRows::Different);
        assert_eq!(sizing.rows_shown(Px(700), measure(&calls)), Fill::Wait);
        assert_eq!(calls.get(), 1);
    }

    /// Rows that change measure nothing; the wait that the first of them
    /// starts measures once for the whole burst, and fills only for a cut
    /// value.
    #[test]
    fn a_burst_of_updates_measures_once_after_the_wait() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(true);
        sizing.resized(Px(800), measure(&calls));
        assert_eq!(sizing.rows_shown(Px(800), measure(&calls)), Fill::Wait);
        assert_eq!(sizing.rows_shown(Px(800), measure(&calls)), Fill::Keep);
        assert_eq!(calls.get(), 1);
        let current = [Px(200), Px(600)];
        assert_eq!(sizing.waited(&current, Px(800), measure(&calls)), None);
        assert_eq!(calls.get(), 2);
    }

    /// Each new top row, which a key such as Page Down also brings, starts
    /// the wait again, and a draw from the same row starts nothing, so
    /// drawing a row again never measures.
    #[test]
    fn a_scroll_starts_the_wait_again_at_each_new_top_row() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(true);
        sizing.resized(Px(800), measure(&calls));
        sizing.drawn_from(0);
        assert!(!sizing.drawn_from(0));
        assert!(sizing.drawn_from(36));
        assert!(sizing.drawn_from(72));
        assert_eq!(calls.get(), 1);
    }

    /// A longer value fills a column at its fit after the wait; one that
    /// the person dragged narrower than its fit keeps its width.
    #[test]
    fn a_cut_value_fills_only_a_column_as_wide_as_its_fit() {
        let mut sizing = ColumnSizing::new(true);
        sizing.resized(Px(370), || vec![fit(300), whole(70)]);
        let longer = || vec![fit(300), whole(90)];
        sizing.rows_shown(Px(370), longer);
        assert!(sizing.waited(&[Px(300), Px(70)], Px(370), longer).is_some());
        sizing.rows_shown(Px(370), longer);
        assert_eq!(sizing.waited(&[Px(300), Px(60)], Px(370), longer), None);
    }

    /// With Auto fit off nothing fills by itself; switching it on fills.
    #[test]
    fn auto_fit_off_keeps_every_width_until_switched_on() {
        let calls = Cell::new(0);
        let mut sizing = ColumnSizing::new(false);
        assert_eq!(sizing.resized(Px(800), measure(&calls)), None);
        sizing.new_rows(NewRows::Order);
        assert_eq!(sizing.rows_shown(Px(800), measure(&calls)), Fill::Keep);
        assert_eq!(calls.get(), 0);
        assert!(
            sizing
                .set_automatic(true, Px(800), measure(&calls))
                .is_some()
        );
    }

    /// A column shown again gets the width it had, at the DPI of now.
    #[test]
    fn a_column_shown_again_gets_its_width_back() {
        let mut sizing = ColumnSizing::new(true);
        sizing.column_hidden(Column::Size, Px(90), dpi(96));
        assert_eq!(sizing.column_shown(Column::Size, dpi(192)), Some(Px(180)));
        assert_eq!(sizing.column_shown(Column::Path, dpi(96)), None);
    }
}
