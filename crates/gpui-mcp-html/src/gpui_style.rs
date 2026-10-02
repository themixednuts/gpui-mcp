//! Typed computed styles (htmlswap's lowering) onto GPUI styles.
//!
//! Each field maps to the GPUI style it means, without parsing. Values GPUI
//! cannot draw are left unset here and reported by [`limits`], so the
//! renderer and diagnostics agree on what is supported.

use std::collections::HashSet;

use gpui::{
    AbsoluteLength, AlignContent, AlignItems, BoxShadow, CursorStyle, DefiniteLength,
    FlexDirection, FlexWrap, FontFallbacks, FontStyle, FontWeight, Length, Overflow, SharedString,
    StrikethroughStyle, Styled, TextAlign as GpuiTextAlign, UnderlineStyle, Visibility, point, px,
    relative, rgba,
};
use htmlswap::computed::{
    Align, BorderStyle, BoxSizing, ComputedStyle, Cursor, DecorationStyle, DecorationThickness,
    Display, Distribute, FlexDirection as CssFlexDirection, FlexWrap as CssFlexWrap, FontFamily,
    FontStyle as CssFontStyle, GridAutoFlow, GridLine, LengthAuto, LengthPercentage, LineHeight,
    Overflow as CssOverflow, Position, RepeatCount, Rgba, Size, TextAlign, TextOverflow, TextWrap,
    Track, TrackBreadth, TrackSize, Visibility as CssVisibility, WhiteSpace,
};

/// The GPUI color for a computed color.
pub(crate) fn color(color: Rgba) -> gpui::Hsla {
    rgba(color.to_u32()).into()
}

fn definite(length: LengthPercentage) -> Option<DefiniteLength> {
    match (length.as_px(), length.as_fraction()) {
        (Some(pixels), _) => Some(px(pixels).into()),
        (None, Some(fraction)) => Some(relative(fraction)),
        // `calc()` mixing a length and a percentage.
        (None, None) => None,
    }
}

fn length(value: LengthAuto) -> Option<Length> {
    match value {
        LengthAuto::Auto => Some(Length::Auto),
        LengthAuto::Length(length) => definite(length).map(Length::Definite),
    }
}

/// A size as GPUI holds it.
enum GpuiSize {
    Length(Length),
    /// `max-width: none`: no limit, which GPUI expresses by leaving it unset.
    Unlimited,
}

fn size(value: Size) -> Option<GpuiSize> {
    match value {
        Size::Auto => Some(GpuiSize::Length(Length::Auto)),
        Size::None => Some(GpuiSize::Unlimited),
        Size::Length(length) => definite(length).map(|length| GpuiSize::Length(length.into())),
        Size::MinContent | Size::MaxContent | Size::FitContent => None,
    }
}

/// What `content-box` sizing adds to a size on one axis: the padding and
/// drawn border widths, which GPUI (always border-box) needs included.
/// `None` when the padding is relative, so no absolute size can include it.
fn content_box_extra(style: &ComputedStyle, horizontal: bool) -> Option<f32> {
    if matches!(style.box_sizing, Some(BoxSizing::BorderBox)) {
        return Some(0.0);
    }
    let (padding, sides) = if horizontal {
        (
            [style.padding.left, style.padding.right],
            [
                (style.border_width.left, style.border_style.left),
                (style.border_width.right, style.border_style.right),
            ],
        )
    } else {
        (
            [style.padding.top, style.padding.bottom],
            [
                (style.border_width.top, style.border_style.top),
                (style.border_width.bottom, style.border_style.bottom),
            ],
        )
    };
    let mut extra = 0.0;
    for padding in padding.into_iter().flatten() {
        extra += padding.as_px()?;
    }
    for (width, line) in sides {
        if !matches!(line, None | Some(BorderStyle::None | BorderStyle::Hidden)) {
            extra += width.unwrap_or(0.0);
        }
    }
    Some(extra)
}

/// A size in GPUI's border-box terms. `None` when GPUI cannot express it.
fn box_size(value: Size, extra: Option<f32>) -> Option<GpuiSize> {
    match (value, extra) {
        (Size::Length(length), Some(extra)) if extra != 0.0 => length
            .as_px()
            .map(|pixels| GpuiSize::Length(px(pixels + extra).into())),
        (Size::Length(_), None) => None,
        _ => size(value),
    }
}

fn absolute(length: LengthPercentage) -> Option<AbsoluteLength> {
    length.as_px().map(|pixels| px(pixels).into())
}

const fn align(value: Align) -> Option<AlignItems> {
    Some(match value {
        Align::Start | Align::SelfStart | Align::Left => AlignItems::Start,
        Align::End | Align::SelfEnd | Align::Right => AlignItems::End,
        Align::FlexStart => AlignItems::FlexStart,
        Align::FlexEnd => AlignItems::FlexEnd,
        Align::Center => AlignItems::Center,
        Align::Baseline => AlignItems::Baseline,
        Align::Stretch | Align::Normal => AlignItems::Stretch,
        Align::Auto | Align::LastBaseline => return None,
    })
}

const fn distribute(value: Distribute) -> Option<AlignContent> {
    Some(match value {
        Distribute::Start | Distribute::Left => AlignContent::Start,
        Distribute::End | Distribute::Right => AlignContent::End,
        Distribute::FlexStart => AlignContent::FlexStart,
        Distribute::FlexEnd => AlignContent::FlexEnd,
        Distribute::Center => AlignContent::Center,
        Distribute::Stretch => AlignContent::Stretch,
        Distribute::SpaceBetween => AlignContent::SpaceBetween,
        Distribute::SpaceAround => AlignContent::SpaceAround,
        Distribute::SpaceEvenly => AlignContent::SpaceEvenly,
        Distribute::Normal => return None,
    })
}

const fn overflow(value: CssOverflow) -> Overflow {
    match value {
        CssOverflow::Visible => Overflow::Visible,
        CssOverflow::Hidden => Overflow::Hidden,
        CssOverflow::Clip => Overflow::Clip,
        CssOverflow::Scroll | CssOverflow::Auto => Overflow::Scroll,
    }
}

const fn cursor(value: Cursor) -> Option<CursorStyle> {
    Some(match value {
        Cursor::Auto | Cursor::Default => CursorStyle::Arrow,
        Cursor::Pointer => CursorStyle::PointingHand,
        Cursor::Text => CursorStyle::IBeam,
        Cursor::VerticalText => CursorStyle::IBeamCursorForVerticalLayout,
        Cursor::Crosshair | Cursor::Cell => CursorStyle::Crosshair,
        Cursor::Grab => CursorStyle::OpenHand,
        Cursor::Grabbing | Cursor::Move | Cursor::AllScroll => CursorStyle::ClosedHand,
        Cursor::NotAllowed | Cursor::NoDrop => CursorStyle::OperationNotAllowed,
        Cursor::Alias => CursorStyle::DragLink,
        Cursor::Copy => CursorStyle::DragCopy,
        Cursor::EwResize => CursorStyle::ResizeLeftRight,
        Cursor::NsResize => CursorStyle::ResizeUpDown,
        Cursor::NeswResize => CursorStyle::ResizeUpRightDownLeft,
        Cursor::NwseResize => CursorStyle::ResizeUpLeftDownRight,
        Cursor::ColResize => CursorStyle::ResizeColumn,
        Cursor::RowResize => CursorStyle::ResizeRow,
        Cursor::ContextMenu
        | Cursor::Help
        | Cursor::Progress
        | Cursor::Wait
        | Cursor::None
        | Cursor::ZoomIn
        | Cursor::ZoomOut => return None,
    })
}

/// The font family GPUI should use: the first installed family in the list,
/// with the rest as fallbacks. Generic families map to the system UI font.
fn font_family(families: &[FontFamily], available: &HashSet<String>) -> (String, Vec<String>) {
    let names = families
        .iter()
        .map(|family| match family {
            FontFamily::Named(name) => name.to_string(),
            _ => ".SystemUIFont".to_owned(),
        })
        .collect::<Vec<_>>();
    let selected = names
        .iter()
        .position(|name| name == ".SystemUIFont" || available.contains(&name.to_ascii_lowercase()));
    match selected {
        Some(index) => (names[index].clone(), names[index + 1..].to_vec()),
        None => (".SystemUIFont".to_owned(), Vec::new()),
    }
}

/// Apply every field the style sets.
#[allow(clippy::too_many_lines)]
pub(crate) fn apply<T: Styled>(mut host: T, style: &ComputedStyle, fonts: &HashSet<String>) -> T {
    match style.display {
        Some(Display::None) => host = host.hidden(),
        Some(Display::Flex | Display::InlineFlex) => host = host.flex(),
        Some(Display::Grid | Display::InlineGrid) => host = host.grid(),
        Some(Display::Block | Display::Inline | Display::InlineBlock | Display::FlowRoot) => {
            host = host.block();
        }
        Some(Display::Contents | Display::Other) | None => {}
    }
    match style.position {
        Some(Position::Absolute) => host = host.absolute(),
        Some(Position::Relative | Position::Static) => host = host.relative(),
        Some(Position::Fixed | Position::Sticky) | None => {}
    }
    {
        let inset = &mut host.style().inset;
        for (slot, value) in [
            (&mut inset.top, style.inset.top),
            (&mut inset.right, style.inset.right),
            (&mut inset.bottom, style.inset.bottom),
            (&mut inset.left, style.inset.left),
        ] {
            if let Some(value) = value.and_then(length) {
                *slot = Some(value);
            }
        }
    }
    {
        let horizontal = content_box_extra(style, true);
        let vertical = content_box_extra(style, false);
        let refinement = host.style();
        for (slot, value, extra) in [
            (&mut refinement.size.width, style.width, horizontal),
            (&mut refinement.size.height, style.height, vertical),
            (&mut refinement.min_size.width, style.min_width, horizontal),
            (&mut refinement.min_size.height, style.min_height, vertical),
            (&mut refinement.max_size.width, style.max_width, horizontal),
            (&mut refinement.max_size.height, style.max_height, vertical),
        ] {
            if let Some(GpuiSize::Length(value)) = value.and_then(|value| box_size(value, extra)) {
                *slot = Some(value);
            }
        }
        if style.aspect_ratio.is_some() {
            refinement.aspect_ratio = style.aspect_ratio;
        }
        let margin = &mut refinement.margin;
        for (slot, value) in [
            (&mut margin.top, style.margin.top),
            (&mut margin.right, style.margin.right),
            (&mut margin.bottom, style.margin.bottom),
            (&mut margin.left, style.margin.left),
        ] {
            if let Some(value) = value.and_then(length) {
                *slot = Some(value);
            }
        }
        let padding = &mut refinement.padding;
        for (slot, value) in [
            (&mut padding.top, style.padding.top),
            (&mut padding.right, style.padding.right),
            (&mut padding.bottom, style.padding.bottom),
            (&mut padding.left, style.padding.left),
        ] {
            if let Some(value) = value.and_then(definite) {
                *slot = Some(value);
            }
        }
        if let Some(direction) = style.flex_direction {
            refinement.flex_direction = Some(match direction {
                CssFlexDirection::Row => FlexDirection::Row,
                CssFlexDirection::RowReverse => FlexDirection::RowReverse,
                CssFlexDirection::Column => FlexDirection::Column,
                CssFlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
            });
        }
        if let Some(wrap) = style.flex_wrap {
            refinement.flex_wrap = Some(match wrap {
                CssFlexWrap::NoWrap => FlexWrap::NoWrap,
                CssFlexWrap::Wrap => FlexWrap::Wrap,
                CssFlexWrap::WrapReverse => FlexWrap::WrapReverse,
            });
        }
        if let Some(grow) = style.flex_grow {
            refinement.flex_grow = Some(grow);
        }
        if let Some(shrink) = style.flex_shrink {
            refinement.flex_shrink = Some(shrink);
        }
        if let Some(Some(GpuiSize::Length(basis))) = style.flex_basis.map(size) {
            refinement.flex_basis = Some(basis);
        }
        if let Some(value) = style.align_items {
            refinement.align_items = align(value);
        }
        if let Some(value) = style.align_self {
            refinement.align_self = align(value);
        }
        if let Some(value) = style.align_content.and_then(distribute) {
            refinement.align_content = Some(value);
        }
        if let Some(value) = style.justify_content.and_then(distribute) {
            refinement.justify_content = Some(value);
        }
        if let Some(gap) = style.column_gap.and_then(definite) {
            refinement.gap.width = Some(gap);
        }
        if let Some(gap) = style.row_gap.and_then(definite) {
            refinement.gap.height = Some(gap);
        }
        if let Some(value) = style.overflow_x {
            refinement.overflow.x = Some(overflow(value));
        }
        if let Some(value) = style.overflow_y {
            refinement.overflow.y = Some(overflow(value));
        }
        if let Some(visibility) = style.visibility {
            refinement.visibility = Some(match visibility {
                CssVisibility::Visible => Visibility::Visible,
                CssVisibility::Hidden | CssVisibility::Collapse => Visibility::Hidden,
            });
        }
        if let Some(opacity) = style.opacity {
            refinement.opacity = Some(opacity);
        }
        if let Some(value) = style.cursor.and_then(cursor) {
            refinement.mouse_cursor = Some(value);
        }
    }
    host = apply_grid(host, style);
    if let Some(background) = style.background_color {
        host = host.bg(color(background));
    }
    host = apply_borders(host, style);
    if let Some(shadows) = &style.box_shadow {
        host.style().box_shadow = Some(
            shadows
                .iter()
                .map(|shadow| BoxShadow {
                    color: color(shadow.color),
                    offset: point(px(shadow.x), px(shadow.y)),
                    blur_radius: px(shadow.blur),
                    spread_radius: px(shadow.spread),
                    inset: shadow.inset,
                })
                .collect(),
        );
    }
    apply_text(host, style, fonts)
}

fn apply_borders<T: Styled>(mut host: T, style: &ComputedStyle) -> T {
    let sides = [
        (style.border_width.top, style.border_style.top),
        (style.border_width.right, style.border_style.right),
        (style.border_width.bottom, style.border_style.bottom),
        (style.border_width.left, style.border_style.left),
    ];
    let mut dashed = false;
    {
        let widths = &mut host.style().border_widths;
        let slots = [
            &mut widths.top,
            &mut widths.right,
            &mut widths.bottom,
            &mut widths.left,
        ];
        for (slot, (width, line)) in slots.into_iter().zip(sides) {
            // A border is drawn only with a visible style; its width alone
            // does not draw it (the initial style is `none`).
            match (width, line) {
                (_, Some(BorderStyle::None | BorderStyle::Hidden)) => *slot = Some(px(0.).into()),
                (Some(width), Some(_)) => *slot = Some(px(width).into()),
                (Some(_), None) | (None, _) => {}
            }
            dashed |= matches!(line, Some(BorderStyle::Dashed | BorderStyle::Dotted));
        }
    }
    if dashed {
        host = host.border_dashed();
    }
    let colors = [
        style.border_color.top,
        style.border_color.right,
        style.border_color.bottom,
        style.border_color.left,
    ];
    if let Some(first) = colors.iter().flatten().next() {
        host = host.border_color(color(*first));
    }
    let radii = &mut host.style().corner_radii;
    for (slot, value) in [
        (&mut radii.top_left, style.border_radius.top_left),
        (&mut radii.top_right, style.border_radius.top_right),
        (&mut radii.bottom_right, style.border_radius.bottom_right),
        (&mut radii.bottom_left, style.border_radius.bottom_left),
    ] {
        if let Some(value) = value.and_then(absolute) {
            *slot = Some(value);
        }
    }
    host
}

fn apply_grid<T: Styled>(mut host: T, style: &ComputedStyle) -> T {
    if let Some(tracks) = style.grid_template_columns.as_deref().and_then(grid_tracks) {
        host = host.grid_template_columns(tracks);
    }
    if let Some(tracks) = style.grid_template_rows.as_deref().and_then(grid_tracks) {
        host = host.grid_template_rows(tracks);
    }
    if let Some(sizes) = style.grid_auto_columns.as_deref().and_then(track_sizes) {
        host = host.grid_auto_columns(sizes);
    }
    if let Some(sizes) = style.grid_auto_rows.as_deref().and_then(track_sizes) {
        host = host.grid_auto_rows(sizes);
    }
    if let Some(flow) = style.grid_auto_flow {
        host = host.grid_auto_flow(auto_flow(flow));
    }
    let location = host.style().grid_location_mut();
    for (slot, value) in [
        (&mut location.column.start, style.grid_column_start),
        (&mut location.column.end, style.grid_column_end),
        (&mut location.row.start, style.grid_row_start),
        (&mut location.row.end, style.grid_row_end),
    ] {
        if let Some(value) = value {
            *slot = placement(value);
        }
    }
    host
}

const fn auto_flow(flow: GridAutoFlow) -> gpui::GridAutoFlow {
    match (flow.column, flow.dense) {
        (false, false) => gpui::GridAutoFlow::Row,
        (true, false) => gpui::GridAutoFlow::Column,
        (false, true) => gpui::GridAutoFlow::RowDense,
        (true, true) => gpui::GridAutoFlow::ColumnDense,
    }
}

const fn placement(line: GridLine) -> gpui::GridPlacement {
    match line {
        GridLine::Auto => gpui::GridPlacement::Auto,
        GridLine::Line(line) => gpui::GridPlacement::Line(line),
        GridLine::Span(span) => gpui::GridPlacement::Span(span),
    }
}

fn breadth(value: TrackBreadth) -> Option<gpui::GridTrackBreadth> {
    Some(match value {
        TrackBreadth::Length(length) => gpui::GridTrackBreadth::Length(definite(length)?),
        TrackBreadth::Flex(fraction) => gpui::GridTrackBreadth::Fraction(fraction),
        TrackBreadth::MinContent => gpui::GridTrackBreadth::MinContent,
        TrackBreadth::MaxContent => gpui::GridTrackBreadth::MaxContent,
        TrackBreadth::Auto => gpui::GridTrackBreadth::Auto,
    })
}

fn track_size(value: TrackSize) -> Option<gpui::GridTrackSize> {
    Some(match value {
        TrackSize::Breadth(value) => gpui::GridTrackSize::Breadth(breadth(value)?),
        TrackSize::MinMax(min, max) => gpui::GridTrackSize::MinMax(breadth(min)?, breadth(max)?),
        TrackSize::FitContent(limit) => gpui::GridTrackSize::FitContent(definite(limit)?),
    })
}

fn track_sizes(values: &[TrackSize]) -> Option<Vec<gpui::GridTrackSize>> {
    values.iter().copied().map(track_size).collect()
}

fn grid_tracks(tracks: &[Track]) -> Option<Vec<gpui::GridTrack>> {
    tracks
        .iter()
        .map(|track| {
            Some(match track {
                Track::Size(size) => gpui::GridTrack::Single(track_size(*size)?),
                Track::Repeat { count, tracks } => gpui::GridTrack::Repeat(
                    match count {
                        RepeatCount::Count(count) => gpui::GridRepetition::Count(*count),
                        RepeatCount::AutoFill => gpui::GridRepetition::AutoFill,
                        RepeatCount::AutoFit => gpui::GridRepetition::AutoFit,
                    },
                    track_sizes(tracks)?,
                ),
            })
        })
        .collect()
}

fn apply_text<T: Styled>(mut host: T, style: &ComputedStyle, fonts: &HashSet<String>) -> T {
    if let Some(text) = style.color {
        host = host.text_color(color(text));
    }
    if let Some(families) = &style.font_family {
        let (primary, fallbacks) = font_family(families, fonts);
        let text = host.text_style();
        text.font_family = Some(SharedString::from(primary));
        text.font_fallbacks = (!fallbacks.is_empty()).then(|| FontFallbacks::from_fonts(fallbacks));
    }
    if let Some(size) = style.font_size {
        host = host.text_size(px(size));
    }
    if let Some(weight) = style.font_weight {
        host = host.font_weight(FontWeight(weight));
    }
    if let Some(font_style) = style.font_style {
        host.text_style().font_style = Some(match font_style {
            CssFontStyle::Normal => FontStyle::Normal,
            CssFontStyle::Italic => FontStyle::Italic,
            CssFontStyle::Oblique => FontStyle::Oblique,
        });
    }
    match style.line_height {
        Some(LineHeight::Number(factor)) => host = host.line_height(relative(factor)),
        Some(LineHeight::Px(height)) => host = host.line_height(px(height)),
        Some(LineHeight::Normal) | None => {}
    }
    match style.text_align {
        Some(TextAlign::Start | TextAlign::Left) => {
            host.text_style().text_align = Some(GpuiTextAlign::Left);
        }
        Some(TextAlign::End | TextAlign::Right) => {
            host.text_style().text_align = Some(GpuiTextAlign::Right);
        }
        Some(TextAlign::Center) => host.text_style().text_align = Some(GpuiTextAlign::Center),
        Some(TextAlign::Justify) | None => {}
    }
    let no_wrap = matches!(
        style.white_space,
        Some(WhiteSpace::NoWrap | WhiteSpace::Pre)
    ) || style.text_wrap == Some(TextWrap::NoWrap);
    if no_wrap {
        host = host.whitespace_nowrap();
    } else if style.white_space.is_some() || style.text_wrap.is_some() {
        host = host.whitespace_normal();
    }
    if style.text_overflow == Some(TextOverflow::Ellipsis) {
        host = host.text_ellipsis();
    }
    if let Some(clamp) = style.line_clamp {
        host.text_style().line_clamp = clamp.map(|lines| lines as usize);
    }
    if let Some(lines) = style.text_decoration_line {
        let wavy = style.text_decoration_style == Some(DecorationStyle::Wavy);
        let decoration_color = style.text_decoration_color.map(color);
        let thickness = match style.text_decoration_thickness {
            Some(DecorationThickness::Px(thickness)) => px(thickness),
            _ => px(1.),
        };
        let text = host.text_style();
        text.underline = lines.underline.then_some(UnderlineStyle {
            thickness,
            color: decoration_color,
            wavy,
        });
        text.strikethrough = lines.line_through.then_some(StrikethroughStyle {
            thickness,
            color: decoration_color,
        });
    }
    host
}

/// Values in `style` that GPUI cannot draw, as `(property, reason)`.
#[allow(clippy::too_many_lines)]
pub(crate) fn limits(style: &ComputedStyle) -> Vec<(&'static str, &'static str)> {
    const MIXED: &str =
        "GPUI lengths are either absolute or relative; calc() mixing both is not supported";
    let mut limits = Vec::new();
    let mut push = |property, reason| {
        if !limits.contains(&(property, reason)) {
            limits.push((property, reason));
        }
    };
    let mixed =
        |value: Option<LengthPercentage>| value.is_some_and(|value| definite(value).is_none());
    let mixed_auto = |value: Option<LengthAuto>| matches!(value, Some(LengthAuto::Length(value)) if definite(value).is_none());
    let content_size = |value: Option<Size>| {
        matches!(
            value,
            Some(Size::MinContent | Size::MaxContent | Size::FitContent)
        )
    };
    if matches!(style.display, Some(Display::Contents | Display::Other)) {
        push("display", "GPUI has no contents, table or ruby display");
    }
    if matches!(style.position, Some(Position::Fixed | Position::Sticky)) {
        push("position", "GPUI has no fixed or sticky positioning");
    }
    for (property, value) in [
        ("width", style.width),
        ("height", style.height),
        ("min-width", style.min_width),
        ("min-height", style.min_height),
        ("max-width", style.max_width),
        ("max-height", style.max_height),
        ("flex-basis", style.flex_basis),
    ] {
        if content_size(value) {
            push(
                property,
                "GPUI has no intrinsic (min-content, max-content, fit-content) sizes",
            );
        }
        if matches!(value, Some(Size::Length(length)) if definite(length).is_none()) {
            push(property, MIXED);
        }
    }
    for (property, value, horizontal) in [
        ("width", style.width, true),
        ("height", style.height, false),
        ("min-width", style.min_width, true),
        ("min-height", style.min_height, false),
        ("max-width", style.max_width, true),
        ("max-height", style.max_height, false),
    ] {
        let Some(Size::Length(length)) = value else {
            continue;
        };
        let extra = content_box_extra(style, horizontal);
        if definite(length).is_some()
            && extra.is_none_or(|extra| extra != 0.0 && length.as_px().is_none())
        {
            push(
                property,
                "GPUI sizes are border-box; a relative content-box size with padding or borders needs box-sizing: border-box",
            );
        }
    }
    for value in [
        style.inset.top,
        style.inset.right,
        style.inset.bottom,
        style.inset.left,
    ] {
        if mixed_auto(value) {
            push("inset", MIXED);
        }
    }
    for value in [
        style.margin.top,
        style.margin.right,
        style.margin.bottom,
        style.margin.left,
    ] {
        if mixed_auto(value) {
            push("margin", MIXED);
        }
    }
    for value in [
        style.padding.top,
        style.padding.right,
        style.padding.bottom,
        style.padding.left,
    ] {
        if mixed(value) {
            push("padding", MIXED);
        }
    }
    if mixed(style.row_gap) || mixed(style.column_gap) {
        push("gap", MIXED);
    }
    if style.justify_items.is_some() || style.justify_self.is_some() {
        push("justify-items", "GPUI has no justify-items or justify-self");
    }
    if matches!(style.align_items, Some(Align::LastBaseline))
        || matches!(style.align_self, Some(Align::LastBaseline))
    {
        push("align-items", "GPUI has no last-baseline alignment");
    }
    let styles = [
        style.border_style.top,
        style.border_style.right,
        style.border_style.bottom,
        style.border_style.left,
    ];
    if styles.iter().flatten().any(|line| {
        matches!(
            line,
            BorderStyle::Double
                | BorderStyle::Groove
                | BorderStyle::Ridge
                | BorderStyle::Inset
                | BorderStyle::Outset
        )
    }) {
        push("border-style", "GPUI draws solid and dashed borders only");
    }
    if styles
        .iter()
        .flatten()
        .any(|line| matches!(line, BorderStyle::Dotted))
    {
        push("border-style", "dotted borders are drawn dashed");
    }
    let colors = [
        style.border_color.top,
        style.border_color.right,
        style.border_color.bottom,
        style.border_color.left,
    ];
    let mut set = colors.iter().flatten();
    if let Some(first) = set.next()
        && set.any(|other| other != first)
    {
        push("border-color", "GPUI draws one border color for all sides");
    }
    for corner in [
        style.border_radius.top_left,
        style.border_radius.top_right,
        style.border_radius.bottom_right,
        style.border_radius.bottom_left,
    ] {
        if corner.is_some_and(|corner| corner.as_px().is_none()) {
            push("border-radius", "GPUI corner radii are absolute lengths");
        }
    }
    if matches!(style.cursor, Some(cursor) if self::cursor(cursor).is_none()) {
        push("cursor", "GPUI has no cursor of this kind");
    }
    if style.letter_spacing.is_some_and(|spacing| spacing != 0.0) {
        push("letter-spacing", "GPUI text has no letter spacing");
    }
    if style.word_spacing.is_some_and(|spacing| spacing != 0.0) {
        push("word-spacing", "GPUI text has no word spacing");
    }
    if style.text_align == Some(TextAlign::Justify) {
        push("text-align", "GPUI text cannot be justified");
    }
    if matches!(
        style.text_wrap,
        Some(TextWrap::Balance | TextWrap::Pretty | TextWrap::Stable)
    ) {
        push(
            "text-wrap",
            "GPUI wraps greedily; balance and pretty wrap normally",
        );
    }
    if style
        .text_decoration_line
        .is_some_and(|lines| lines.overline)
    {
        push("text-decoration", "GPUI text has no overline");
    }
    if matches!(
        style.text_decoration_style,
        Some(DecorationStyle::Double | DecorationStyle::Dotted | DecorationStyle::Dashed)
    ) {
        push(
            "text-decoration-style",
            "GPUI draws solid and wavy text decorations only",
        );
    }
    limits
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui::{Styled, div, px};
    use htmlswap::StyleDeclaration;
    use htmlswap::computed::{ComputedScope, ComputedStyle, FontFamily, MediaEnvironment};

    use super::{apply, font_family};

    fn computed(declarations: &[(&str, &str)]) -> ComputedStyle {
        let declarations = declarations
            .iter()
            .map(|(property, value)| StyleDeclaration::new(*property, *value, false, None))
            .collect::<Vec<_>>();
        let root = ComputedScope::root(&MediaEnvironment::default());
        let scope = root.child(declarations.iter());
        ComputedStyle::compute(&declarations, &scope.style_context(&root), |_, _| {})
    }

    fn top_border(declarations: &[(&str, &str)]) -> Option<gpui::AbsoluteLength> {
        let mut host = apply(div(), &computed(declarations), &HashSet::new());
        host.style().border_widths.top
    }

    #[test]
    fn borders_draw_only_with_a_visible_style() {
        assert_eq!(top_border(&[("border-width", "1px")]), None);
        assert_eq!(
            top_border(&[("border-width", "1px"), ("border-style", "solid")]),
            Some(px(1.).into())
        );
        assert_eq!(
            top_border(&[("border-width", "1px"), ("border-style", "dashed")]),
            Some(px(1.).into())
        );
        assert_eq!(
            top_border(&[("border", "1px solid red"), ("border-style", "hidden")]),
            Some(px(0.).into())
        );
    }

    #[test]
    fn content_box_sizes_include_padding_and_drawn_borders() {
        let width = |declarations: &[(&str, &str)]| {
            let mut host = apply(div(), &computed(declarations), &HashSet::new());
            host.style().size.width
        };
        let px_width = |pixels: f32| Some(gpui::Length::Definite(px(pixels).into()));

        assert_eq!(
            width(&[
                ("width", "100px"),
                ("padding", "10px"),
                ("border", "2px solid")
            ]),
            px_width(124.0)
        );
        assert_eq!(
            width(&[
                ("width", "100px"),
                ("padding", "10px"),
                ("border-width", "2px")
            ]),
            px_width(120.0),
            "a border without a style is not drawn and takes no space"
        );
        assert_eq!(
            width(&[
                ("box-sizing", "border-box"),
                ("width", "100px"),
                ("padding", "10px")
            ]),
            px_width(100.0)
        );
        assert_eq!(
            width(&[("width", "50%"), ("padding", "10px")]),
            None,
            "GPUI cannot add padding to a relative size; diagnosed instead"
        );
        assert_eq!(
            width(&[("width", "50%")]),
            Some(gpui::Length::Definite(gpui::relative(0.5)))
        );
    }

    #[test]
    fn font_family_picks_the_first_installed_family_and_keeps_case() {
        let available = HashSet::from(["segoe ui".to_owned()]);
        let families = |css: &str| -> Vec<FontFamily> {
            computed(&[("font-family", css)])
                .font_family
                .unwrap_or_default()
        };

        assert_eq!(
            font_family(&families("'Segoe UI', sans-serif"), &available),
            ("Segoe UI".to_owned(), vec![".SystemUIFont".to_owned()])
        );
        assert_eq!(
            font_family(&families("system-ui"), &available),
            (".SystemUIFont".to_owned(), Vec::new())
        );
        assert_eq!(
            font_family(&families("'Missing Font', sans-serif"), &available),
            (".SystemUIFont".to_owned(), Vec::new())
        );
    }
}
