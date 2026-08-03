use gpui::{
    App, Keystroke, Modifiers, MouseButton as GpuiMouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PlatformInput, ScrollDelta, ScrollWheelEvent, TouchPhase, Window, point, px,
};
use gpui_mcp_protocol::{
    BridgeError, ErrorCode, InputCommand, MAX_TEXT_BYTES, MouseButton, NativeInputCommand,
    NativeScrollDelta, Point, SemanticAction,
};

pub(crate) fn validate(command: &InputCommand) -> Result<(), BridgeError> {
    match command {
        InputCommand::Click { point, count, .. } => {
            validate_point(*point)?;
            validate_click_count(*count)?;
        }
        InputCommand::Hover { point } => validate_point(*point)?,
        InputCommand::Drag { from, to, steps } => {
            validate_point(*from)?;
            validate_point(*to)?;
            if !(1..=120).contains(steps) {
                return Err(invalid("drag steps must be between 1 and 120"));
            }
        }
        InputCommand::Key { keystroke } => {
            validate_text(keystroke)?;
            Keystroke::parse(keystroke).map_err(|_| invalid("invalid GPUI keystroke syntax"))?;
        }
        InputCommand::TypeText { text } | InputCommand::ReplaceText { text } => {
            validate_text(text)?;
        }
        InputCommand::Scroll {
            point,
            delta_x,
            delta_y,
        } => {
            validate_point(*point)?;
            validate_scroll_delta(*delta_x, *delta_y)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_native(command: &NativeInputCommand) -> Result<(), BridgeError> {
    match command {
        NativeInputCommand::MouseMove { point, .. } => validate_point(*point),
        NativeInputCommand::MouseDown {
            point, click_count, ..
        }
        | NativeInputCommand::MouseUp {
            point, click_count, ..
        } => {
            validate_point(*point)?;
            validate_click_count(*click_count)
        }
        NativeInputCommand::ScrollWheel { point, delta } => {
            validate_point(*point)?;
            let (delta_x, delta_y) = match delta {
                NativeScrollDelta::Pixels { delta_x, delta_y }
                | NativeScrollDelta::Lines { delta_x, delta_y } => (*delta_x, *delta_y),
            };
            validate_scroll_delta(delta_x, delta_y)
        }
    }
}

pub(crate) fn validate_semantic(action: &SemanticAction) -> Result<(), BridgeError> {
    match action {
        SemanticAction::Click { count, .. } => validate_click_count(*count)?,
        SemanticAction::Focus | SemanticAction::Hover => {}
        SemanticAction::Drag { to, steps } => {
            validate_point(*to)?;
            if !(1..=120).contains(steps) {
                return Err(invalid("drag steps must be between 1 and 120"));
            }
        }
        SemanticAction::Scroll { delta_x, delta_y } => {
            validate_scroll_delta(*delta_x, *delta_y)?;
        }
        SemanticAction::SetText { text } => validate_text(text)?,
        SemanticAction::SetValue { value } => validate_text(value)?,
    }
    Ok(())
}

pub(crate) fn dispatch_native(
    command: &NativeInputCommand,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), BridgeError> {
    validate_native(command)?;
    let event = match command {
        NativeInputCommand::MouseMove {
            point: position,
            pressed_button,
        } => PlatformInput::MouseMove(MouseMoveEvent {
            position: native_point(*position),
            pressed_button: pressed_button.map(native_button),
            modifiers: Modifiers::default(),
        }),
        NativeInputCommand::MouseDown {
            point: position,
            button,
            click_count,
        } => PlatformInput::MouseDown(MouseDownEvent {
            button: native_button(*button),
            position: native_point(*position),
            modifiers: Modifiers::default(),
            click_count: usize::from(*click_count),
            first_mouse: false,
        }),
        NativeInputCommand::MouseUp {
            point: position,
            button,
            click_count,
        } => PlatformInput::MouseUp(MouseUpEvent {
            button: native_button(*button),
            position: native_point(*position),
            modifiers: Modifiers::default(),
            click_count: usize::from(*click_count),
        }),
        NativeInputCommand::ScrollWheel {
            point: position,
            delta,
        } => PlatformInput::ScrollWheel(ScrollWheelEvent {
            position: native_point(*position),
            delta: match delta {
                NativeScrollDelta::Pixels { delta_x, delta_y } => {
                    ScrollDelta::Pixels(point(px(*delta_x), px(*delta_y)))
                }
                NativeScrollDelta::Lines { delta_x, delta_y } => {
                    ScrollDelta::Lines(point(*delta_x, *delta_y))
                }
            },
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        }),
    };
    window.dispatch_event(event, cx);
    Ok(())
}

/// Return the GPUI input pipeline's latest pointer position in window-relative logical pixels.
#[must_use]
pub(crate) fn pointer_location(window: &Window) -> Point {
    let position = window.mouse_position();
    Point {
        x: f32::from(position.x),
        y: f32::from(position.y),
    }
}

pub(crate) fn dispatch_keyboard(
    command: InputCommand,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), BridgeError> {
    validate(&command)?;
    match command {
        InputCommand::Key { keystroke } => {
            let parsed = Keystroke::parse(&keystroke)
                .map_err(|_| invalid("invalid GPUI keystroke syntax"))?;
            window.dispatch_keystroke(parsed, cx);
        }
        InputCommand::TypeText { text } => type_text(&text, window, cx),
        InputCommand::ReplaceText { text } => {
            let select_all = Keystroke::parse("secondary-a")
                .map_err(|_| BridgeError::new(ErrorCode::Internal, "key mapping unavailable"))?;
            window.dispatch_keystroke(select_all, cx);
            if text.is_empty() {
                let backspace = Keystroke::parse("backspace").map_err(|_| {
                    BridgeError::new(ErrorCode::Internal, "key mapping unavailable")
                })?;
                window.dispatch_keystroke(backspace, cx);
            } else {
                type_text(&text, window, cx);
            }
        }
        _ => {
            return Err(BridgeError::new(
                ErrorCode::Unsupported,
                "pointer input requires an annotated GPUI action handler",
            ));
        }
    }
    Ok(())
}

fn type_text(text: &str, window: &mut Window, cx: &mut App) {
    for character in text.chars() {
        let keystroke = match character {
            '\n' | '\r' => Keystroke::parse("enter").ok(),
            '\t' => Keystroke::parse("tab").ok(),
            _ => Some(Keystroke {
                modifiers: Modifiers::default(),
                key: character.to_lowercase().collect(),
                key_char: Some(character.to_string()),
            }),
        };
        if let Some(keystroke) = keystroke {
            window.dispatch_keystroke(keystroke, cx);
        }
    }
}

fn native_point(position: Point) -> gpui::Point<gpui::Pixels> {
    point(px(position.x), px(position.y))
}

const fn native_button(button: MouseButton) -> GpuiMouseButton {
    match button {
        MouseButton::Left => GpuiMouseButton::Left,
        MouseButton::Right => GpuiMouseButton::Right,
        MouseButton::Middle => GpuiMouseButton::Middle,
    }
}

fn validate_click_count(click_count: u8) -> Result<(), BridgeError> {
    if !(1..=3).contains(&click_count) {
        return Err(invalid("click count must be between 1 and 3"));
    }
    Ok(())
}

fn validate_scroll_delta(delta_x: f32, delta_y: f32) -> Result<(), BridgeError> {
    if !delta_x.is_finite() || !delta_y.is_finite() {
        return Err(invalid("scroll deltas must be finite"));
    }
    if delta_x.abs() > 100_000.0 || delta_y.abs() > 100_000.0 {
        return Err(invalid("scroll delta exceeds the safety bound"));
    }
    Ok(())
}

fn validate_point(point: Point) -> Result<(), BridgeError> {
    if !point.is_valid() {
        return Err(invalid("coordinates must be finite"));
    }
    if point.x.abs() > 1_000_000.0 || point.y.abs() > 1_000_000.0 {
        return Err(invalid("coordinates exceed the safety bound"));
    }
    Ok(())
}

fn validate_text(text: &str) -> Result<(), BridgeError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(invalid("text exceeds the 64 KiB safety bound"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> BridgeError {
    BridgeError::new(ErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        AppContext as _, Context, InteractiveElement as _, IntoElement,
        MouseButton as GpuiMouseButton, ParentElement as _, Render,
        StatefulInteractiveElement as _, Styled as _, TestAppContext, Window, div, point, px, size,
    };
    use gpui_mcp_protocol::{MouseButton, NativeInputCommand, Point};

    use super::dispatch_native;

    #[derive(Clone, Copy)]
    struct DragValue;

    struct DragPreview;

    impl Render for DragPreview {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn synthetic_platform_drag_runs_native_mouse_drag_and_drop_handlers(cx: &mut TestAppContext) {
        let pressed = Rc::new(Cell::new(false));
        let drag_started = Rc::new(Cell::new(false));
        let dropped = Rc::new(Cell::new(false));
        let pressed_for_handler = pressed.clone();
        let drag_started_for_handler = drag_started.clone();
        let dropped_for_handler = dropped.clone();
        let visual = cx.add_empty_window();
        visual.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(100.0)),
            move |_, _| {
                div()
                    .flex()
                    .gap(px(50.0))
                    .child(
                        div()
                            .id("native-drag-source")
                            .w(px(100.0))
                            .h(px(100.0))
                            .on_mouse_down(GpuiMouseButton::Left, move |_, _, _| {
                                pressed_for_handler.set(true);
                            })
                            .on_drag(DragValue, move |_, _, _, cx| {
                                drag_started_for_handler.set(true);
                                cx.new(|_| DragPreview)
                            }),
                    )
                    .child(
                        div()
                            .id("native-drop-target")
                            .w(px(100.0))
                            .h(px(100.0))
                            .on_drop(move |_: &DragValue, _, _| {
                                dropped_for_handler.set(true);
                            }),
                    )
            },
        );

        visual.update(|window, cx| {
            assert_eq!(
                dispatch_native(
                    &NativeInputCommand::MouseDown {
                        point: Point { x: 50.0, y: 50.0 },
                        button: MouseButton::Left,
                        click_count: 1,
                    },
                    window,
                    cx,
                ),
                Ok(())
            );
            assert_eq!(
                dispatch_native(
                    &NativeInputCommand::MouseMove {
                        point: Point { x: 60.0, y: 50.0 },
                        pressed_button: Some(MouseButton::Left),
                    },
                    window,
                    cx,
                ),
                Ok(())
            );
            assert_eq!(
                dispatch_native(
                    &NativeInputCommand::MouseMove {
                        point: Point { x: 200.0, y: 50.0 },
                        pressed_button: Some(MouseButton::Left),
                    },
                    window,
                    cx,
                ),
                Ok(())
            );
            assert_eq!(
                dispatch_native(
                    &NativeInputCommand::MouseUp {
                        point: Point { x: 200.0, y: 50.0 },
                        button: MouseButton::Left,
                        click_count: 1,
                    },
                    window,
                    cx,
                ),
                Ok(())
            );
            assert_eq!(window.mouse_position(), point(px(200.0), px(50.0)));
        });

        assert!(pressed.get());
        assert!(drag_started.get());
        assert!(dropped.get());
    }
}
