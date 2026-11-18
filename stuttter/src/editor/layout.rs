//! The window cut into its bands, worked out without drawing them.
//!
//! The editor is one resizable window ([`super::create`]):
//!
//! ```text
//! +--------------------------------------------------------------+
//! | top bar (two rows of controls)                               |
//! +--------------------------------------------------------------+
//! | Time row                                  |  block panel     |
//! | lane bar (Volume / Cutoff / Res, Filter)  |  (the selected   |
//! | Volume row (or Cutoff, or Res)            |   block's        |
//! | waveform (whatever height is left)        |   settings)      |
//! +--------------------------------------------------------------+
//! ```
//!
//! The Time row, the Volume row and the waveform share the left column, so a
//! step, its level and its sound stay above one another at any window size.
//! None of the bands is an egui widget that sizes itself: each is a rectangle
//! something is painted or laid out into, which is what makes the split
//! testable without a display.
//!
//! A window smaller than the bands want — a host ignoring the minimum size —
//! still never produces a negative rectangle or two bands over the same
//! points: every band is clamped to what is left, the waveform losing its
//! height first, and the panel never takes more than half the width.

use egui::{Rect, pos2};

use super::theme;

/// `value`, forced into `min..=max` — including when it is NaN.
///
/// `f32::clamp` hands a NaN straight back; `max` then `min` cannot, because
/// both discard a NaN operand in favour of the bound. Shared with
/// [`super::top_bar`], which cuts the bar's rows into slots the same way.
#[allow(clippy::manual_clamp)]
pub(super) fn sane(value: f32, min: f32, max: f32) -> f32 {
    value.max(min).min(max)
}

/// The bands of one window. Built per frame from the rectangle egui hands
/// the editor; cheap and small enough to copy.
#[derive(Clone, Copy, Debug)]
pub struct EditorLayout {
    full: Rect,
    top_bar: Rect,
    separator: Rect,
    time_row: Rect,
    lane_bar: Rect,
    volume_row: Rect,
    waveform: Rect,
    panel: Rect,
}

impl EditorLayout {
    /// The bands of a window filling `full`.
    pub fn new(full: Rect) -> EditorLayout {
        // The window, squared up: a reversed, NaN or infinite rect cannot
        // reach the arithmetic below.
        let left = sane(full.min.x, f32::MIN, f32::MAX);
        let top = sane(full.min.y, f32::MIN, f32::MAX);
        let right = left + sane(full.width(), 0.0, f32::MAX);
        let bottom = top + sane(full.height(), 0.0, f32::MAX);
        let full = Rect::from_min_max(pos2(left, top), pos2(right, bottom));

        let bar_bottom = sane(top + theme::TOP_BAR_HEIGHT, top, bottom);
        let line_bottom = sane(bar_bottom + theme::HAIRLINE, bar_bottom, bottom);

        // Everything under the bar, inside the panel's padding. Half of what
        // there is, at most, so a window squeezed to nothing has an empty
        // content rect and not an inverted one.
        let pad_x = sane(theme::PANEL_PADDING, 0.0, (right - left) * 0.5);
        let pad_y = sane(theme::PANEL_PADDING, 0.0, (bottom - line_bottom) * 0.5);
        let content = Rect::from_min_max(
            pos2(left + pad_x, line_bottom + pad_y),
            pos2(right - pad_x, bottom - pad_y),
        );

        // The block panel on the right, never more than half the content;
        // the gap before it only where there is room for one.
        let panel_width = sane(theme::BLOCK_PANEL_WIDTH, 0.0, content.width() * 0.5);
        let gap = sane(theme::COLUMN_GAP, 0.0, content.width() - 2.0 * panel_width);
        let main_right = content.max.x - panel_width - gap;
        let main = Rect::from_min_max(content.min, pos2(main_right, content.max.y));
        let panel_left = content.max.x - panel_width;
        let panel = Rect::from_min_max(pos2(panel_left, content.min.y), content.max);

        let time_row = band(main, 0.0, theme::TIME_ROW_HEIGHT);
        let lane_top = theme::TIME_ROW_HEIGHT + theme::ROW_GAP;
        let lane_bar = band(main, lane_top, theme::LANE_BAR_HEIGHT);
        let volume_top = lane_top + theme::LANE_BAR_HEIGHT + theme::ROW_GAP;
        let volume_row = band(main, volume_top, theme::VOLUME_ROW_HEIGHT);
        let wave_top = volume_top + theme::VOLUME_ROW_HEIGHT + theme::ROW_GAP;
        let waveform = band(main, wave_top, f32::MAX);

        EditorLayout {
            full,
            top_bar: Rect::from_min_max(pos2(left, top), pos2(right, bar_bottom)),
            separator: Rect::from_min_max(pos2(left, bar_bottom), pos2(right, line_bottom)),
            time_row,
            lane_bar,
            volume_row,
            waveform,
            panel,
        }
    }

    /// The whole window, with any negative or NaN size squared up.
    pub fn full(&self) -> Rect {
        self.full
    }

    /// The top bar, padding and all.
    pub fn top_bar(&self) -> Rect {
        self.top_bar
    }

    /// The room inside the top bar its controls are laid out in.
    pub fn bar_controls(&self) -> Rect {
        let pad_x = sane(theme::PANEL_PADDING, 0.0, self.top_bar.width() * 0.5);
        let pad_y = sane(theme::PANEL_PADDING, 0.0, self.top_bar.height() * 0.5);
        self.top_bar.shrink2(egui::vec2(pad_x, pad_y))
    }

    /// Row `row` of the bar's controls: row 0 the pattern strip, row 1
    /// everything else. A row past the last is an empty rect on the bar's
    /// bottom edge.
    pub fn bar_row(&self, row: usize) -> Rect {
        let controls = self.bar_controls();
        if row as f32 >= theme::BAR_ROWS {
            let bottom = controls.max.y;
            return Rect::from_min_max(pos2(controls.min.x, bottom), controls.max);
        }

        band(controls, row as f32 * theme::ROW_PITCH, theme::CONTROL_HEIGHT)
    }

    /// The hairline under the bar.
    pub fn separator(&self) -> Rect {
        self.separator
    }

    /// The Time row: the edited pattern's steps and blocks.
    pub fn time_row(&self) -> Rect {
        self.time_row
    }

    /// The strip over the Volume row: which of the pattern's three rows of
    /// levels it shows, and the pattern's filter.
    pub fn lane_bar(&self) -> Rect {
        self.lane_bar
    }

    /// The Volume row, on the Time row's columns.
    pub fn volume_row(&self) -> Rect {
        self.volume_row
    }

    /// The waveform view, on the same columns, down to the window's bottom.
    pub fn waveform(&self) -> Rect {
        self.waveform
    }

    /// The block panel on the right: the selected block's settings.
    pub fn panel(&self) -> Rect {
        self.panel
    }
}

/// A band `height` tall, `offset` below the top of `content`, clamped inside
/// it at both ends.
fn band(content: Rect, offset: f32, height: f32) -> Rect {
    let top = sane(content.min.y + offset, content.min.y, content.max.y);
    let bottom = sane(top + height, top, content.max.y);
    Rect::from_min_max(pos2(content.min.x, top), pos2(content.max.x, bottom))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{DEFAULT_HEIGHT, DEFAULT_WIDTH, GRID_WIDTH, MIN_HEIGHT, MIN_WIDTH};
    use egui::vec2;

    fn window_of(width: f32, height: f32) -> EditorLayout {
        EditorLayout::new(Rect::from_min_size(pos2(17.0, 5.0), vec2(width, height)))
    }

    fn window() -> EditorLayout {
        window_of(DEFAULT_WIDTH, DEFAULT_HEIGHT)
    }

    fn bands(layout: &EditorLayout) -> [(&'static str, Rect); 6] {
        [
            ("top bar", layout.top_bar()),
            ("Time row", layout.time_row()),
            ("lane bar", layout.lane_bar()),
            ("Volume row", layout.volume_row()),
            ("waveform", layout.waveform()),
            ("panel", layout.panel()),
        ]
    }

    fn band_at(layout: &EditorLayout, pos: egui::Pos2) -> Option<&'static str> {
        bands(layout)
            .into_iter()
            .find(|(_, rect)| rect.is_positive() && rect.contains(pos))
            .map(|(name, _)| name)
    }

    #[test]
    fn the_default_window_gives_every_band_its_own_size() {
        let layout = window();

        assert_eq!(layout.top_bar().height(), theme::TOP_BAR_HEIGHT);
        assert_eq!(layout.separator().height(), theme::HAIRLINE);
        assert_eq!(layout.bar_row(0).height(), theme::CONTROL_HEIGHT);
        assert_eq!(layout.bar_row(1).height(), theme::CONTROL_HEIGHT);
        assert_eq!(layout.time_row().height(), theme::TIME_ROW_HEIGHT);
        assert_eq!(layout.lane_bar().height(), theme::LANE_BAR_HEIGHT);
        assert_eq!(layout.volume_row().height(), theme::VOLUME_ROW_HEIGHT);
        assert_eq!(layout.panel().width(), theme::BLOCK_PANEL_WIDTH);

        for rect in [layout.time_row(), layout.lane_bar(), layout.volume_row(), layout.waveform()] {
            assert_eq!(rect.width(), GRID_WIDTH);
            assert_eq!(rect.min.x, layout.time_row().min.x, "one column");
        }

        // The waveform and the panel run down to the padding over the bottom.
        let floor = layout.full().max.y - theme::PANEL_PADDING;
        assert_eq!(layout.waveform().max.y, floor);
        assert_eq!(layout.panel().max.y, floor);
        assert!(layout.waveform().height() > 2.0 * theme::TIME_ROW_HEIGHT);
    }

    #[test]
    fn the_column_and_the_panel_grow_with_the_window() {
        let small = window_of(MIN_WIDTH, MIN_HEIGHT);
        let large = window_of(DEFAULT_WIDTH + 300.0, DEFAULT_HEIGHT + 200.0);

        assert!(large.time_row().width() > small.time_row().width());
        assert!(large.waveform().height() > small.waveform().height());
        assert_eq!(large.time_row().height(), small.time_row().height());
        assert_eq!(large.panel().width(), small.panel().width(), "the panel keeps its width");
        assert!(small.time_row().width() >= 16.0 * 20.0, "sixteen usable cells at the minimum");
    }

    #[test]
    fn the_bars_controls_are_cut_into_rows() {
        let layout = window();
        let controls = layout.bar_controls();

        assert_eq!(layout.bar_row(0).min.y, controls.min.y);
        assert_eq!(layout.bar_row(1).min.y - layout.bar_row(0).max.y, theme::ROW_GAP);
        assert_eq!(layout.bar_row(1).max.y, controls.max.y);
        for row in [2, 3, usize::MAX] {
            let past = layout.bar_row(row);
            assert!(!past.is_positive(), "row {row}");
            assert!(controls.contains_rect(past), "row {row}");
        }
    }

    #[test]
    fn the_bands_are_stacked_in_order_with_the_themes_gaps() {
        let layout = window();

        assert_eq!(layout.top_bar().max.y, layout.separator().min.y);
        assert_eq!(layout.time_row().min.y - layout.separator().max.y, theme::PANEL_PADDING);
        assert_eq!(layout.lane_bar().min.y - layout.time_row().max.y, theme::ROW_GAP);
        assert_eq!(layout.volume_row().min.y - layout.lane_bar().max.y, theme::ROW_GAP);
        assert_eq!(layout.waveform().min.y - layout.volume_row().max.y, theme::ROW_GAP);
        assert_eq!(layout.panel().min.x - layout.time_row().max.x, theme::COLUMN_GAP);
        assert_eq!(layout.panel().min.y, layout.time_row().min.y);

        for (name, rect) in bands(&layout) {
            assert!(layout.full().contains_rect(rect), "{name} is outside the window");
        }
    }

    #[test]
    fn the_bands_never_overlap_and_the_gaps_belong_to_nobody() {
        let layout = window();

        let all = bands(&layout);
        for (index, &(name, rect)) in all.iter().enumerate() {
            for &(other, other_rect) in all.iter().skip(index + 1) {
                assert!(!rect.intersect(other_rect).is_positive(), "{name} and {other} overlap");
            }
        }
        for (name, rect) in all {
            assert_eq!(band_at(&layout, rect.center()), Some(name), "{name}");
        }

        let left = layout.full().min.x;
        let hairline = pos2(left + 10.0, layout.separator().center().y);
        let row_gap = pos2(left + 10.0, layout.time_row().max.y + theme::ROW_GAP * 0.5);
        let column_gap = pos2(layout.time_row().max.x + 1.0, layout.time_row().center().y);
        for pos in [hairline, row_gap, column_gap] {
            assert_eq!(band_at(&layout, pos), None, "{pos:?} is in a band");
        }
    }

    #[test]
    fn a_squeezed_window_empties_the_bands_from_the_bottom() {
        let origin = pos2(0.0, 0.0);

        let shorter = theme::TOP_BAR_HEIGHT + theme::HAIRLINE + theme::TIME_ROW_HEIGHT;
        let layout = EditorLayout::new(Rect::from_min_size(origin, vec2(DEFAULT_WIDTH, shorter)));
        assert!(layout.time_row().height() > 0.0, "a row of cells is the last thing dropped");
        assert_eq!(layout.lane_bar().height(), 0.0);
        assert_eq!(layout.volume_row().height(), 0.0);
        assert_eq!(layout.waveform().height(), 0.0);

        let narrow = EditorLayout::new(Rect::from_min_size(origin, vec2(200.0, DEFAULT_HEIGHT)));
        assert!(narrow.panel().width() <= narrow.time_row().width() + 1e-3, "at most half");

        for size in [vec2(0.0, 0.0), vec2(DEFAULT_WIDTH, 0.0), vec2(0.0, DEFAULT_HEIGHT)] {
            let layout = EditorLayout::new(Rect::from_min_size(origin, size));
            for (name, rect) in bands(&layout) {
                assert!(rect.width() >= 0.0 && rect.height() >= 0.0, "{name} at {size:?}");
                assert!(layout.full().contains_rect(rect), "{name} at {size:?}");
            }
        }
    }

    #[test]
    fn a_nonsense_window_is_squared_up() {
        let reversed = Rect::from_min_max(pos2(500.0, 400.0), pos2(100.0, 50.0));
        let nan = Rect::from_min_size(pos2(f32::NAN, f32::NAN), vec2(f32::NAN, f32::NAN));

        for full in [reversed, Rect::EVERYTHING, Rect::NOTHING, nan] {
            let layout = EditorLayout::new(full);
            for (name, rect) in bands(&layout) {
                assert!(rect.min.x.is_finite() && rect.min.y.is_finite(), "{name} at {full:?}");
                assert!(rect.max.x.is_finite() && rect.max.y.is_finite(), "{name} at {full:?}");
                assert!(rect.width() >= 0.0 && rect.height() >= 0.0, "{name} at {full:?}");
                assert!(layout.full().contains_rect(rect), "{name} at {full:?}");
            }
        }
    }
}
