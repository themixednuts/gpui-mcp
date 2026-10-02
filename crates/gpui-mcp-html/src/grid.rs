//! CSS grid for the live renderer: track lists, implicit tracks, auto-flow,
//! line placement and spans, mapped onto GPUI's grid track styles.

use std::ops::Range;

use gpui::{
    DefiniteLength, GridAutoFlow, GridPlacement, GridRepetition, GridTrack, GridTrackBreadth,
    GridTrackSize, Styled, px,
};
use htmlswap::StyleProperty;

/// A grid property the live renderer implements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GridProperty {
    TemplateColumns,
    TemplateRows,
    AutoColumns,
    AutoRows,
    AutoFlow,
    Column,
    Row,
    ColumnStart,
    ColumnEnd,
    RowStart,
    RowEnd,
    RowGap,
    ColumnGap,
}

impl GridProperty {
    /// Recognize a grid property, including those htmlswap passes through by name.
    pub(crate) fn of(property: &StyleProperty) -> Option<Self> {
        Some(match property {
            StyleProperty::GridTemplateColumns => Self::TemplateColumns,
            StyleProperty::GridColumn => Self::Column,
            StyleProperty::Unknown(name) => match name.as_str() {
                "grid-template-rows" => Self::TemplateRows,
                "grid-auto-columns" => Self::AutoColumns,
                "grid-auto-rows" => Self::AutoRows,
                "grid-auto-flow" => Self::AutoFlow,
                "grid-row" => Self::Row,
                "grid-column-start" => Self::ColumnStart,
                "grid-column-end" => Self::ColumnEnd,
                "grid-row-start" => Self::RowStart,
                "grid-row-end" => Self::RowEnd,
                "row-gap" => Self::RowGap,
                "column-gap" => Self::ColumnGap,
                _ => return None,
            },
            _ => return None,
        })
    }

    /// What the renderer accepts, for a diagnostic when a value is rejected.
    pub(crate) const fn accepted(self) -> &'static str {
        match self {
            Self::TemplateColumns | Self::TemplateRows => {
                "live renderer accepts track lists of lengths, percentages, fr, auto, min-content, max-content, minmax(), fit-content() and repeat(n | auto-fill | auto-fit, …), without line names"
            }
            Self::AutoColumns | Self::AutoRows => {
                "live renderer accepts implicit track sizes without repeat()"
            }
            Self::AutoFlow => {
                "live renderer accepts row, column, row dense, column dense and dense"
            }
            Self::Column | Self::Row => {
                "live renderer accepts auto, grid line numbers, and span counts, as start / end"
            }
            Self::ColumnStart | Self::ColumnEnd | Self::RowStart | Self::RowEnd => {
                "live renderer accepts auto, a grid line number, or a span count"
            }
            Self::RowGap | Self::ColumnGap => "live renderer accepts a length or percentage",
        }
    }

    /// Whether `value` is one the renderer applies.
    pub(crate) fn accepts(self, value: &str) -> bool {
        match self {
            Self::TemplateColumns | Self::TemplateRows => track_list(value).is_some(),
            Self::AutoColumns | Self::AutoRows => auto_tracks(value).is_some(),
            Self::AutoFlow => auto_flow(value).is_some(),
            Self::Column | Self::Row => placement_range(value).is_some(),
            Self::ColumnStart | Self::ColumnEnd | Self::RowStart | Self::RowEnd => {
                placement(value).is_some()
            }
            Self::RowGap | Self::ColumnGap => gap(value).is_some(),
        }
    }

    /// Apply `value`, leaving `host` unchanged when the value is not accepted.
    pub(crate) fn apply<T: Styled>(self, mut host: T, value: &str) -> T {
        match self {
            Self::TemplateColumns => {
                if let Some(tracks) = track_list(value) {
                    host = host.grid_template_columns(tracks);
                }
            }
            Self::TemplateRows => {
                if let Some(tracks) = track_list(value) {
                    host = host.grid_template_rows(tracks);
                }
            }
            Self::AutoColumns => {
                if let Some(tracks) = auto_tracks(value) {
                    host = host.grid_auto_columns(tracks);
                }
            }
            Self::AutoRows => {
                if let Some(tracks) = auto_tracks(value) {
                    host = host.grid_auto_rows(tracks);
                }
            }
            Self::AutoFlow => {
                if let Some(flow) = auto_flow(value) {
                    host = host.grid_auto_flow(flow);
                }
            }
            Self::Column => {
                if let Some(range) = placement_range(value) {
                    host = host.grid_column(range.start, range.end);
                }
            }
            Self::Row => {
                if let Some(range) = placement_range(value) {
                    host = host.grid_row(range.start, range.end);
                }
            }
            Self::ColumnStart | Self::ColumnEnd | Self::RowStart | Self::RowEnd => {
                if let Some(line) = placement(value) {
                    let location = host.style().grid_location_mut();
                    match self {
                        Self::ColumnStart => location.column.start = line,
                        Self::ColumnEnd => location.column.end = line,
                        Self::RowStart => location.row.start = line,
                        _ => location.row.end = line,
                    }
                }
            }
            Self::RowGap => {
                if let Some(gap) = gap(value) {
                    host = host.gap_y(gap);
                }
            }
            Self::ColumnGap => {
                if let Some(gap) = gap(value) {
                    host = host.gap_x(gap);
                }
            }
        }
        host
    }
}

/// Parse a `grid-template-*` track list.
pub(crate) fn track_list(value: &str) -> Option<Vec<GridTrack>> {
    let value = value.trim();
    if value.is_empty() || value == "none" || value.contains('[') {
        return None;
    }
    let mut tracks = Vec::new();
    for part in split_top_level(value) {
        if let Some(arguments) = function(part, "repeat") {
            let (count, sizes) = arguments.split_once(',')?;
            let count = match count.trim() {
                "auto-fill" => GridRepetition::AutoFill,
                "auto-fit" => GridRepetition::AutoFit,
                count => GridRepetition::Count(count.parse::<u16>().ok().filter(|n| *n > 0)?),
            };
            let sizes = split_top_level(sizes)
                .into_iter()
                .map(track_size)
                .collect::<Option<Vec<_>>>()?;
            if sizes.is_empty() {
                return None;
            }
            tracks.push(GridTrack::Repeat(count, sizes));
        } else {
            tracks.push(GridTrack::Single(track_size(part)?));
        }
    }
    (!tracks.is_empty()).then_some(tracks)
}

/// Parse a `grid-auto-*` list of implicit track sizes.
fn auto_tracks(value: &str) -> Option<Vec<GridTrackSize>> {
    let sizes = split_top_level(value.trim())
        .into_iter()
        .map(track_size)
        .collect::<Option<Vec<_>>>()?;
    (!sizes.is_empty()).then_some(sizes)
}

/// Parse one track size: a breadth, `minmax(min, max)` or `fit-content(limit)`.
pub(crate) fn track_size(value: &str) -> Option<GridTrackSize> {
    let value = value.trim();
    if let Some(arguments) = function(value, "minmax") {
        let (min, max) = arguments.split_once(',')?;
        let min = breadth(min.trim())?;
        // A flexible minimum is invalid CSS.
        if matches!(min, GridTrackBreadth::Fraction(_)) {
            return None;
        }
        return Some(GridTrackSize::MinMax(min, breadth(max.trim())?));
    }
    if let Some(limit) = function(value, "fit-content") {
        return length(limit.trim()).map(GridTrackSize::FitContent);
    }
    breadth(value).map(GridTrackSize::Breadth)
}

fn breadth(value: &str) -> Option<GridTrackBreadth> {
    Some(match value {
        "auto" => GridTrackBreadth::Auto,
        "min-content" => GridTrackBreadth::MinContent,
        "max-content" => GridTrackBreadth::MaxContent,
        _ => match value.strip_suffix("fr") {
            Some(fraction) => GridTrackBreadth::Fraction(
                fraction
                    .parse::<f32>()
                    .ok()
                    .filter(|f| f.is_finite() && *f >= 0.0)?,
            ),
            None => GridTrackBreadth::Length(length(value)?),
        },
    })
}

fn length(value: &str) -> Option<DefiniteLength> {
    if value == "0" {
        return Some(px(0.).into());
    }
    let length: DefiniteLength = value.try_into().ok()?;
    let negative = match length {
        DefiniteLength::Absolute(_) => value.starts_with('-'),
        DefiniteLength::Fraction(fraction) => fraction < 0.0,
    };
    (!negative).then_some(length)
}

fn gap(value: &str) -> Option<DefiniteLength> {
    length(value.trim())
}

/// Parse `grid-auto-flow`.
pub(crate) fn auto_flow(value: &str) -> Option<GridAutoFlow> {
    let words: Vec<_> = value.split_whitespace().collect();
    Some(match words.as_slice() {
        ["row"] => GridAutoFlow::Row,
        ["column"] => GridAutoFlow::Column,
        ["dense"] | ["row", "dense"] | ["dense", "row"] => GridAutoFlow::RowDense,
        ["column", "dense"] | ["dense", "column"] => GridAutoFlow::ColumnDense,
        _ => return None,
    })
}

/// Parse `grid-column` / `grid-row`: `start [/ end]`.
pub(crate) fn placement_range(value: &str) -> Option<Range<GridPlacement>> {
    let (start, end) = value
        .split_once('/')
        .map_or((value.trim(), "auto"), |(start, end)| {
            (start.trim(), end.trim())
        });
    Some(placement(start)?..placement(end)?)
}

/// Parse one grid line: `auto`, a nonzero line number, or `span n`.
pub(crate) fn placement(value: &str) -> Option<GridPlacement> {
    let value = value.trim();
    if value == "auto" {
        return Some(GridPlacement::Auto);
    }
    if let Some(span) = value.strip_prefix("span ") {
        return span
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|span| *span > 0)
            .map(GridPlacement::Span);
    }
    value
        .parse::<i16>()
        .ok()
        .filter(|line| *line != 0)
        .map(GridPlacement::Line)
}

/// The arguments of `name(…)`, when `value` is exactly that call.
fn function<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// Split on whitespace outside parentheses.
fn split_top_level(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut depth = 0u32;
    for (index, character) in value.char_indices() {
        match character {
            '(' => {
                depth = depth.saturating_add(1);
                start.get_or_insert(index);
            }
            ')' => depth = depth.saturating_sub(1),
            _ if character.is_whitespace() && depth == 0 => {
                if let Some(start_index) = start.take() {
                    parts.push(&value[start_index..index]);
                }
            }
            _ => {
                start.get_or_insert(index);
            }
        }
    }
    if let Some(start) = start {
        parts.push(&value[start..]);
    }
    parts
}

#[cfg(test)]
mod tests {
    use gpui::{
        GridAutoFlow, GridPlacement, GridRepetition, GridTrack, GridTrackBreadth, GridTrackSize,
        px, relative,
    };
    use htmlswap::StyleProperty;

    use super::{GridProperty, auto_flow, placement_range, track_list, track_size};

    #[test]
    fn track_lists_cover_css_track_sizing() {
        assert_eq!(
            track_list("200px 1fr minmax(120px, 2fr) auto 25%"),
            Some(vec![
                GridTrackSize::length(px(200.)).into(),
                GridTrackSize::fr(1.).into(),
                GridTrackSize::minmax(
                    GridTrackBreadth::Length(px(120.).into()),
                    GridTrackBreadth::Fraction(2.)
                )
                .into(),
                GridTrackSize::auto().into(),
                GridTrackSize::length(relative(0.25)).into(),
            ])
        );
        assert_eq!(
            track_list("repeat(auto-fill, minmax(100px, 1fr))"),
            Some(vec![GridTrack::Repeat(
                GridRepetition::AutoFill,
                vec![GridTrackSize::minmax(
                    GridTrackBreadth::Length(px(100.).into()),
                    GridTrackBreadth::Fraction(1.)
                )]
            )])
        );
        assert_eq!(
            track_list("repeat(3, min-content max-content)"),
            Some(vec![GridTrack::repeat(
                3,
                [GridTrackSize::min_content(), GridTrackSize::max_content()]
            )])
        );
        assert_eq!(
            track_size("fit-content(240px)"),
            Some(GridTrackSize::fit_content(px(240.)))
        );
    }

    #[test]
    fn invalid_track_lists_are_rejected() {
        for value in [
            "",
            "none",
            "[sidebar] 200px",
            "repeat(0, 1fr)",
            "repeat(2)",
            "minmax(1fr, 200px)",
            "-10px",
            "1fr wide",
            "repeat(auto-fit, )",
        ] {
            assert_eq!(track_list(value), None, "{value:?}");
        }
    }

    #[test]
    fn placement_flow_and_property_names() {
        assert_eq!(
            placement_range("2 / span 3"),
            Some(GridPlacement::Line(2)..GridPlacement::Span(3))
        );
        assert_eq!(placement_range("0"), None);
        assert_eq!(auto_flow("dense column"), Some(GridAutoFlow::ColumnDense));
        assert_eq!(auto_flow("sideways"), None);
        assert_eq!(
            GridProperty::of(&StyleProperty::Unknown("grid-template-rows".into())),
            Some(GridProperty::TemplateRows)
        );
        assert_eq!(
            GridProperty::of(&StyleProperty::Unknown("grid-area".into())),
            None
        );
        assert!(GridProperty::RowGap.accepts("12px"));
        assert!(!GridProperty::RowGap.accepts("-1px"));
    }
}
