use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId as GpuiElementId,
    ElementInputHandler, EntityInputHandler, FocusHandle, Focusable, Global, GlobalElementId,
    InteractiveElement as _, IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement as _, Pixels, Point, Render, ShapedLine,
    SharedString, Size, Style, Styled as _, TextRun, UTF16Selection, Window, actions, div, fill,
    point, px, relative, rgba,
};

use crate::render::dispatch_input_change;
use crate::{Binding, ElementId, HookRegistry};

actions!(
    runtime_text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Enter,
        Paste,
        Cut,
        Copy,
    ]
);

struct InputInitialized;
impl Global for InputInitialized {}

/// Initialize keyboard actions used by native inputs in live HTML documents.
///
/// Calling this more than once for the same [`App`] is a no-op.
pub fn init(cx: &mut App) {
    if cx.has_global::<InputInitialized>() {
        return;
    }
    cx.set_global(InputInitialized);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, None),
        KeyBinding::new("delete", Delete, None),
        KeyBinding::new("left", Left, None),
        KeyBinding::new("right", Right, None),
        KeyBinding::new("shift-left", SelectLeft, None),
        KeyBinding::new("shift-right", SelectRight, None),
        KeyBinding::new("cmd-a", SelectAll, None),
        KeyBinding::new("cmd-v", Paste, None),
        KeyBinding::new("cmd-c", Copy, None),
        KeyBinding::new("cmd-x", Cut, None),
        KeyBinding::new("home", Home, None),
        KeyBinding::new("end", End, None),
        KeyBinding::new("enter", Enter, None),
    ]);
}

pub(crate) struct RuntimeTextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selection: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_lines: Vec<StoredLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_line_height: Pixels,
    selecting: bool,
    behavior: InputBehavior,
    document_revision: u64,
    element_id: ElementId,
    bindings: Vec<Binding>,
    hooks: HookRegistry,
}

#[derive(Clone, Copy)]
struct InputBehavior {
    multiline: bool,
    masked: bool,
    disabled: bool,
}

#[derive(Clone)]
pub(crate) struct RuntimeTextInputOptions {
    pub value: String,
    pub placeholder: String,
    pub multiline: bool,
    pub masked: bool,
    pub disabled: bool,
    pub document_revision: u64,
    pub element_id: ElementId,
    pub bindings: Vec<Binding>,
    pub hooks: HookRegistry,
}

struct StoredLine {
    source: Range<usize>,
    shaped: ShapedLine,
    origin: Point<Pixels>,
}

impl RuntimeTextInput {
    pub(crate) fn new(
        options: RuntimeTextInputOptions,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let value = normalize_value(&options.value, options.multiline);
        let caret = value.len();
        Self {
            focus_handle: cx.focus_handle(),
            content: value.into(),
            placeholder: options.placeholder.into(),
            selection: caret..caret,
            selection_reversed: false,
            marked_range: None,
            last_lines: Vec::new(),
            last_bounds: None,
            last_line_height: px(0.),
            selecting: false,
            behavior: InputBehavior {
                multiline: options.multiline,
                masked: options.masked,
                disabled: options.disabled,
            },
            document_revision: options.document_revision,
            element_id: options.element_id,
            bindings: options.bindings,
            hooks: options.hooks,
        }
    }

    pub(crate) fn sync(
        &mut self,
        options: RuntimeTextInputOptions,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = normalize_value(&options.value, options.multiline);
        if self.content.as_ref() != value {
            self.content = value.into();
            let caret = self.content.len();
            self.selection = caret..caret;
            self.selection_reversed = false;
            self.marked_range = None;
            self.last_lines.clear();
        }
        self.placeholder = options.placeholder.into();
        self.behavior.disabled = options.disabled;
        self.document_revision = options.document_revision;
        self.element_id = options.element_id;
        self.bindings = options.bindings;
        self.hooks = options.hooks;
        cx.notify();
    }

    pub(crate) const fn is_compatible(&self, multiline: bool, masked: bool) -> bool {
        self.behavior.multiline == multiline && self.behavior.masked == masked
    }

    pub(crate) fn needs_sync(
        &self,
        document_revision: u64,
        value: &str,
        placeholder: &str,
        disabled: bool,
        _cx: &App,
    ) -> bool {
        self.document_revision != document_revision
            || self.content.as_ref() != normalize_value(value, self.behavior.multiline)
            || self.placeholder.as_ref() != placeholder
            || self.behavior.disabled != disabled
    }

    pub(crate) fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }

    fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = floor_char_boundary(&self.content, offset.min(self.content.len()));
        self.selection = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = floor_char_boundary(&self.content, offset.min(self.content.len()));
        if self.selection_reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.selection_reversed = !self.selection_reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content[..floor_char_boundary(&self.content, offset)]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        let offset = ceil_char_boundary(&self.content, offset);
        self.content[offset..]
            .char_indices()
            .nth(1)
            .map_or(self.content.len(), |(index, _)| offset + index)
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_to(self.previous_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selection.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_to(self.next_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selection.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor();
        let start = self.content[..cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.move_to(start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor();
        let end = self.content[cursor..]
            .find('\n')
            .map_or(self.content.len(), |index| cursor + index);
        self.move_to(end, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.behavior.disabled {
            return;
        }
        if self.selection.is_empty() {
            let previous = self.previous_boundary(self.cursor());
            if previous == self.cursor() {
                window.play_system_bell();
                return;
            }
            self.select_to(previous, cx);
        }
        self.replace(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.behavior.disabled {
            return;
        }
        if self.selection.is_empty() {
            let next = self.next_boundary(self.cursor());
            if next == self.cursor() {
                window.play_system_bell();
                return;
            }
            self.select_to(next, cx);
        }
        self.replace(None, "", window, cx);
    }

    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.behavior.multiline && !self.behavior.disabled {
            self.replace(None, "\n", window, cx);
        } else {
            cx.propagate();
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.behavior.disabled {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.behavior.masked && !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selection.clone()].to_owned(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.behavior.disabled || self.behavior.masked || self.selection.is_empty() {
            return;
        }
        self.copy(&Copy, window, cx);
        self.replace(None, "", window, cx);
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.behavior.disabled {
            return;
        }
        window.focus(&self.focus_handle, cx);
        self.selecting = true;
        let index = self.index_for_point(event.position);
        if event.modifiers.shift {
            self.select_to(index, cx);
        } else {
            self.move_to(index, cx);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_for_point(event.position), cx);
        }
    }

    fn index_for_point(&self, point: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        if self.last_lines.is_empty() {
            return 0;
        }
        let line_height = self.last_line_height;
        if line_height <= px(0.) {
            return 0;
        }
        let Some(line) = self
            .last_lines
            .iter()
            .find(|line| point.y < line.origin.y + line_height)
            .or_else(|| self.last_lines.last())
        else {
            return 0;
        };
        let display_index = line
            .shaped
            .closest_index_for_x((point.x - bounds.left()).max(px(0.)));
        source_index_for_display(
            &self.content,
            &line.source,
            display_index,
            self.behavior.masked,
        )
    }

    fn replace(
        &mut self,
        range_utf16: Option<&Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selection.clone());
        let text = normalize_value(text, self.behavior.multiline);
        let mut content = String::with_capacity(self.content.len() - range.len() + text.len());
        content.push_str(&self.content[..range.start]);
        content.push_str(&text);
        content.push_str(&self.content[range.end..]);
        let caret = range.start + text.len();
        self.content = content.into();
        self.selection = caret..caret;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_lines.clear();
        cx.notify();
        let _ = dispatch_input_change(
            &self.hooks,
            &self.element_id,
            &self.bindings,
            self.content.to_string(),
            window,
            cx,
        );
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for character in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf8 += character.len_utf8();
            utf16 += character.len_utf16();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..floor_char_boundary(&self.content, offset)]
            .encode_utf16()
            .count()
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
}

impl Focusable for RuntimeTextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for RuntimeTextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.behavior.masked {
            return None;
        }
        let range = self.range_from_utf16(&range);
        adjusted.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if self.behavior.disabled && !ignore_disabled_input {
            return None;
        }
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selection),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.behavior.disabled {
            self.replace(range.as_ref(), text, window, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.behavior.disabled {
            return;
        }
        let insertion_start = range
            .as_ref()
            .map(|range| self.range_from_utf16(range).start)
            .or_else(|| self.marked_range.as_ref().map(|range| range.start))
            .unwrap_or(self.selection.start);
        self.replace(range.as_ref(), text, window, cx);
        if !text.is_empty() {
            self.marked_range = Some(insertion_start..insertion_start + text.len());
        }
        if let Some(selected) = selected {
            let selected = self.range_from_utf16(&selected);
            self.selection = insertion_start + selected.start..insertion_start + selected.end;
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range);
        let line = self
            .last_lines
            .iter()
            .find(|line| line.source.start <= range.start && range.start <= line.source.end)?;
        let line_height = self.last_line_height;
        if line_height <= px(0.) {
            return None;
        }
        let start = display_index_for_source(
            &self.content,
            &line.source,
            range.start,
            self.behavior.masked,
        );
        let end = display_index_for_source(
            &self.content,
            &line.source,
            range.end.min(line.source.end),
            self.behavior.masked,
        );
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.shaped.x_for_index(start),
                line.origin.y,
            ),
            point(
                bounds.left() + line.shaped.x_for_index(end),
                line.origin.y + line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_point(point)))
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        (!self.behavior.masked).then(|| self.content.encode_utf16().count())
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.behavior.disabled
    }
}

impl Render for RuntimeTextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .key_context("RuntimeTextInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}

struct TextElement {
    input: gpui::Entity<RuntimeTextInput>,
}

struct PrepaintState {
    lines: Vec<PaintedLine>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    line_height: Pixels,
}

struct PaintedLine {
    source: Range<usize>,
    shaped: ShapedLine,
    origin: Point<Pixels>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<GpuiElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = if input.behavior.multiline {
            relative(1.).into()
        } else {
            window.line_height().into()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let ranges = source_lines(&input.content);
        let line_count = ranges.len().max(1);
        let line_height = if input.behavior.multiline {
            window.line_height()
        } else {
            bounds.size.height
        };
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let mut lines = Vec::with_capacity(line_count);
        let mut selections = Vec::new();
        let mut cursor = None;

        let mut origin_y = bounds.top();
        for source in ranges {
            let is_placeholder = input.content.is_empty();
            let display = if is_placeholder {
                input.placeholder.to_string()
            } else {
                display_line(&input.content, &source, input.behavior.masked)
            };
            let run = TextRun {
                len: display.len(),
                font: style.font(),
                color: if is_placeholder {
                    rgba(0x8080_8099).into()
                } else {
                    style.color
                },
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window
                .text_system()
                .shape_line(display.into(), font_size, &[run], None);
            let origin = point(bounds.left(), origin_y);
            origin_y += line_height;

            let selection_start = input.selection.start.max(source.start).min(source.end);
            let selection_end = input.selection.end.max(source.start).min(source.end);
            if selection_start < selection_end {
                let start = display_index_for_source(
                    &input.content,
                    &source,
                    selection_start,
                    input.behavior.masked,
                );
                let end = display_index_for_source(
                    &input.content,
                    &source,
                    selection_end,
                    input.behavior.masked,
                );
                selections.push(fill(
                    Bounds::from_corners(
                        point(origin.x + shaped.x_for_index(start), origin.y),
                        point(origin.x + shaped.x_for_index(end), origin.y + line_height),
                    ),
                    rgba(0x4b8f_ff55),
                ));
            }

            if input.selection.is_empty()
                && source.start <= input.cursor()
                && input.cursor() <= source.end
                && cursor.is_none()
            {
                let index = display_index_for_source(
                    &input.content,
                    &source,
                    input.cursor(),
                    input.behavior.masked,
                );
                cursor = Some(fill(
                    Bounds::new(
                        point(origin.x + shaped.x_for_index(index), origin.y),
                        Size::new(px(1.), line_height),
                    ),
                    style.color,
                ));
            }
            lines.push(PaintedLine {
                source,
                shaped,
                origin,
            });
        }
        PrepaintState {
            lines,
            cursor,
            selections,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in state.selections.drain(..) {
            window.paint_quad(selection);
        }
        if focus.is_focused(window)
            && let Some(cursor) = state.cursor.take()
        {
            window.paint_quad(cursor);
        }
        let line_height = state.line_height;
        let lines = state
            .lines
            .drain(..)
            .map(|line| {
                let _ = line.shaped.paint(
                    line.origin,
                    line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                StoredLine {
                    source: line.source,
                    shaped: line.shaped,
                    origin: line.origin,
                }
            })
            .collect();
        self.input.update(cx, |input, _| {
            input.last_lines = lines;
            input.last_bounds = Some(bounds);
            input.last_line_height = line_height;
        });
    }
}

fn normalize_value(value: &str, multiline: bool) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        normalized
    } else {
        normalized.replace('\n', " ")
    }
}

fn source_lines(content: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, character) in content.char_indices() {
        if character == '\n' {
            lines.push(start..index);
            start = index + 1;
        }
    }
    lines.push(start..content.len());
    lines
}

fn display_line(content: &str, source: &Range<usize>, masked: bool) -> String {
    if masked {
        "•".repeat(content[source.clone()].chars().count())
    } else {
        content[source.clone()].to_owned()
    }
}

fn display_index_for_source(
    content: &str,
    source: &Range<usize>,
    index: usize,
    masked: bool,
) -> usize {
    let index = floor_char_boundary(content, index.clamp(source.start, source.end));
    if masked {
        content[source.start..index].chars().count() * '•'.len_utf8()
    } else {
        index - source.start
    }
}

fn source_index_for_display(
    content: &str,
    source: &Range<usize>,
    index: usize,
    masked: bool,
) -> usize {
    if !masked {
        return floor_char_boundary(content, (source.start + index).min(source.end));
    }
    let characters = index / '•'.len_utf8();
    content[source.clone()]
        .char_indices()
        .nth(characters)
        .map_or(source.end, |(offset, _)| source.start + offset)
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::{
        display_index_for_source, normalize_value, source_index_for_display, source_lines,
    };

    #[test]
    fn single_line_values_normalize_platform_line_endings() {
        assert_eq!(normalize_value("one\r\ntwo\rthree", false), "one two three");
        assert_eq!(normalize_value("one\r\ntwo", true), "one\ntwo");
    }

    #[test]
    fn line_ranges_and_masked_indices_preserve_source_offsets() {
        let value = "aé\n終";
        assert_eq!(source_lines(value), [0..3, 4..7]);
        let line = 0..3;
        let display = display_index_for_source(value, &line, 3, true);
        assert_eq!(source_index_for_display(value, &line, display, true), 3);
    }
}
