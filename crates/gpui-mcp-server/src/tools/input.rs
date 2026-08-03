use super::{
    ClickElementArgs, ClickPointArgs, DragElementArgs, DragPointArgs, ElementArgs, GpuiMcp,
    InputCommand, Json, KeyArgs, MouseButton, NativeClickPointArgs, NativeInputCommand,
    NativeScrollElementArgs, NativeScrollPointArgs, NodeAction, Parameters, Point,
    PointerButtonArgs, PointerMoveArgs, ScrollArgs, SemanticAction, SetTextArgs, SetValueArgs,
    ToolRouter, TypeTextArgs, Value, ack_json, encode_error, get_node, json, object_output,
    require_bounds, tool, tool_router, validate_native_point, validate_value,
};

#[tool_router(router = input_router)]
impl GpuiMcp {
    #[tool(
        description = "Return GPUI's current pointer position in window-relative logical pixels. This is the same pointer state used by pointer_move, pointer_click, pointer_drag, and video cursor overlays; it never reads the global operating-system cursor."
    )]
    async fn pointer_location(&self) -> Result<Json<Value>, String> {
        let point = self.current_pointer_location().await?;
        Ok(object_output(serde_json::json!({
            "x": point.x,
            "y": point.y,
            "coordinate_space": "window_logical_pixels",
        })))
    }

    #[tool(
        description = "Move the real GPUI pointer pipeline to window-relative logical coordinates. This drives native hover and hit testing cross-platform without moving the host OS cursor."
    )]
    async fn pointer_move(
        &self,
        Parameters(args): Parameters<PointerMoveArgs>,
    ) -> Result<Json<Value>, String> {
        let point = Point {
            x: args.x,
            y: args.y,
        };
        validate_native_point(point)?;
        self.dispatch_native_input(NativeInputCommand::MouseMove {
            point,
            pressed_button: args.held_button,
        })
        .await?;
        Ok(ack_json("pointer_moved"))
    }

    #[tool(
        description = "Press a pointer button through GPUI's real native input pipeline at window-relative logical coordinates. Pair with pointer_move and pointer_up for a manually controlled drag."
    )]
    async fn pointer_down(
        &self,
        Parameters(args): Parameters<PointerButtonArgs>,
    ) -> Result<Json<Value>, String> {
        let point = Point {
            x: args.x,
            y: args.y,
        };
        validate_native_point(point)?;
        validate_pointer_click_count(args.count)?;
        self.dispatch_native_input(NativeInputCommand::MouseDown {
            point,
            button: args.button,
            click_count: args.count,
        })
        .await?;
        Ok(ack_json("pointer_down"))
    }

    #[tool(
        description = "Release a pointer button through GPUI's real native input pipeline at window-relative logical coordinates."
    )]
    async fn pointer_up(
        &self,
        Parameters(args): Parameters<PointerButtonArgs>,
    ) -> Result<Json<Value>, String> {
        let point = Point {
            x: args.x,
            y: args.y,
        };
        validate_native_point(point)?;
        validate_pointer_click_count(args.count)?;
        self.dispatch_native_input(NativeInputCommand::MouseUp {
            point,
            button: args.button,
            click_count: args.count,
        })
        .await?;
        Ok(ack_json("pointer_up"))
    }

    #[tool(
        description = "Click through GPUI's real native pointer pipeline at window-relative logical coordinates. This runs normal hit testing and mouse handlers."
    )]
    async fn pointer_click(
        &self,
        Parameters(args): Parameters<PointerButtonArgs>,
    ) -> Result<Json<Value>, String> {
        self.native_click_at(
            Point {
                x: args.x,
                y: args.y,
            },
            args.button,
            args.count,
        )
        .await?;
        Ok(ack_json("pointer_clicked"))
    }

    #[tool(
        description = "Drag through GPUI's real native pointer pipeline between window-relative logical coordinates. This sends settled mouse down, move, and up events so native drag/drop handlers run."
    )]
    async fn pointer_drag(
        &self,
        Parameters(args): Parameters<DragPointArgs>,
    ) -> Result<Json<Value>, String> {
        self.native_drag_between(
            Point {
                x: args.from_x,
                y: args.from_y,
            },
            Point {
                x: args.to_x,
                y: args.to_y,
            },
            args.steps,
        )
        .await?;
        Ok(ack_json("pointer_dragged"))
    }

    #[tool(
        description = "Scroll through GPUI's real native pointer pipeline at window-relative logical coordinates."
    )]
    async fn pointer_scroll(
        &self,
        Parameters(args): Parameters<NativeScrollPointArgs>,
    ) -> Result<Json<Value>, String> {
        self.native_scroll_at(
            Point {
                x: args.x,
                y: args.y,
            },
            args.delta_x,
            args.delta_y,
        )
        .await?;
        Ok(ack_json("pointer_scrolled"))
    }

    #[tool(
        description = "Invoke an annotated semantic click handler directly by stable ID. This does NOT run GPUI native hit testing or mouse handlers; use native_click/native_double_click when the real pointer pipeline matters."
    )]
    async fn click_element(
        &self,
        Parameters(args): Parameters<ClickElementArgs>,
    ) -> Result<Json<Value>, String> {
        self.click_node(&args.id, args.button, args.count).await?;
        Ok(ack_json("clicked"))
    }

    #[tool(
        description = "Invoke an annotated semantic double-click handler directly. This bypasses GPUI native hit testing and mouse handlers; use native_double_click for the real pointer pipeline."
    )]
    async fn double_click_element(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        self.click_node(&args.id, MouseButton::Left, 2).await?;
        Ok(ack_json("double_clicked"))
    }

    #[tool(
        description = "Invoke the topmost compatible annotated semantic click handler under window-relative logical coordinates. This bypasses native hitboxes; use native_click_coordinates for real GPUI pointer input."
    )]
    async fn click_coordinates(
        &self,
        Parameters(args): Parameters<ClickPointArgs>,
    ) -> Result<Json<Value>, String> {
        self.dispatch_input(InputCommand::Click {
            point: Point {
                x: args.x,
                y: args.y,
            },
            button: args.button,
            count: args.count,
        })
        .await?;
        Ok(ack_json("clicked"))
    }

    #[tool(
        description = "Invoke an annotated semantic hover handler directly, without native hit testing. Use native_hover when GPUI mouse-move and hover behavior must run."
    )]
    async fn hover_element(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        self.invoke_node(&tree, &args.id, SemanticAction::Hover)
            .await?;
        Ok(ack_json("hovered"))
    }

    #[tool(
        description = "Invoke an annotated semantic drag handler directly from one element to another. This does NOT run GPUI on_drag/on_drop; use native_drag for the real native drag/drop pipeline."
    )]
    async fn drag_element(
        &self,
        Parameters(args): Parameters<DragElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let to = require_bounds(get_node(&tree, &args.to_id)?)?.center();
        self.invoke_node(
            &tree,
            &args.from_id,
            SemanticAction::Drag {
                to,
                steps: args.steps,
            },
        )
        .await?;
        Ok(ack_json("dragged"))
    }

    #[tool(
        description = "Invoke an annotated semantic drag handler selected by window-relative logical points. This bypasses GPUI native drag/drop; use native_drag_coordinates for native hitboxes and on_drop."
    )]
    async fn drag_coordinates(
        &self,
        Parameters(args): Parameters<DragPointArgs>,
    ) -> Result<Json<Value>, String> {
        self.dispatch_input(InputCommand::Drag {
            from: Point {
                x: args.from_x,
                y: args.from_y,
            },
            to: Point {
                x: args.to_x,
                y: args.to_y,
            },
            steps: args.steps,
        })
        .await?;
        Ok(ack_json("dragged"))
    }

    #[tool(
        description = "Drive a REAL GPUI native left click at an annotated element's live bounds center. Uses in-process PlatformInput hit testing and native mouse handlers; use click_element instead when you intentionally want the semantic handler directly."
    )]
    async fn native_click(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let point = require_bounds(get_node(&tree, &args.id)?)?.center();
        self.native_click_at(point, MouseButton::Left, 1).await?;
        Ok(ack_json("native_clicked"))
    }

    #[tool(
        description = "Drive a REAL GPUI native left-button double click at an annotated element's live bounds center. Dispatches two native down/up pairs through PlatformInput rather than invoking the semantic handler."
    )]
    async fn native_double_click(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let point = require_bounds(get_node(&tree, &args.id)?)?.center();
        self.native_click_at(point, MouseButton::Left, 2).await?;
        Ok(ack_json("native_double_clicked"))
    }

    #[tool(
        description = "Drive a REAL GPUI native mouse move to an annotated element's live bounds center, exercising native hit testing and hover handlers instead of the semantic hover callback."
    )]
    async fn native_hover(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let point = require_bounds(get_node(&tree, &args.id)?)?.center();
        self.dispatch_native_input(NativeInputCommand::MouseMove {
            point,
            pressed_button: None,
        })
        .await?;
        Ok(ack_json("native_hovered"))
    }

    #[tool(
        description = "Drive a REAL GPUI native drag/drop from one annotated element's live center to another. Sends native mouse down, frame-settled interpolated moves crossing GPUI's drag threshold, and mouse up so on_drag/on_drop run; use drag_element for semantic handler dispatch."
    )]
    async fn native_drag(
        &self,
        Parameters(args): Parameters<DragElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let from = require_bounds(get_node(&tree, &args.from_id)?)?.center();
        let to = require_bounds(get_node(&tree, &args.to_id)?)?.center();
        self.native_drag_between(from, to, args.steps).await?;
        Ok(ack_json("native_dragged"))
    }

    #[tool(
        description = "Drive a REAL GPUI native scroll-wheel event at an annotated element's live bounds center. Uses in-process PlatformInput wheel hit testing; use scroll when the semantic scroll callback is the intended target."
    )]
    async fn native_scroll(
        &self,
        Parameters(args): Parameters<NativeScrollElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let point = require_bounds(get_node(&tree, &args.id)?)?.center();
        self.native_scroll_at(point, args.delta_x, args.delta_y)
            .await?;
        Ok(ack_json("native_scrolled"))
    }

    #[tool(
        description = "Drive a REAL GPUI native click at window-relative logical coordinates. Dispatches PlatformInput mouse down/up through native hitboxes; use click_coordinates for annotated semantic handlers."
    )]
    async fn native_click_coordinates(
        &self,
        Parameters(args): Parameters<NativeClickPointArgs>,
    ) -> Result<Json<Value>, String> {
        self.native_click_at(
            Point {
                x: args.x,
                y: args.y,
            },
            args.button,
            1,
        )
        .await?;
        Ok(ack_json("native_clicked"))
    }

    #[tool(
        description = "Drive a REAL GPUI native drag/drop between window-relative logical coordinates. Sends frame-settled PlatformInput down/move/up events through native hitboxes and drag/drop listeners."
    )]
    async fn native_drag_coordinates(
        &self,
        Parameters(args): Parameters<DragPointArgs>,
    ) -> Result<Json<Value>, String> {
        self.native_drag_between(
            Point {
                x: args.from_x,
                y: args.from_y,
            },
            Point {
                x: args.to_x,
                y: args.to_y,
            },
            args.steps,
        )
        .await?;
        Ok(ack_json("native_dragged"))
    }

    #[tool(
        description = "Drive a REAL GPUI native scroll-wheel event at window-relative logical coordinates, exercising native wheel hit testing instead of an annotated semantic scroll callback."
    )]
    async fn native_scroll_coordinates(
        &self,
        Parameters(args): Parameters<NativeScrollPointArgs>,
    ) -> Result<Json<Value>, String> {
        self.native_scroll_at(
            Point {
                x: args.x,
                y: args.y,
            },
            args.delta_x,
            args.delta_y,
        )
        .await?;
        Ok(ack_json("native_scrolled"))
    }

    #[tool(
        description = "Dispatch one GPUI keystroke to the focused element using cross-platform GPUI syntax"
    )]
    async fn keyboard(&self, Parameters(args): Parameters<KeyArgs>) -> Result<Json<Value>, String> {
        self.dispatch_input(InputCommand::Key {
            keystroke: args.keystroke,
        })
        .await?;
        Ok(ack_json("key_dispatched"))
    }

    #[tool(description = "Type UTF-8 text into the currently focused GPUI input")]
    async fn type_text(
        &self,
        Parameters(args): Parameters<TypeTextArgs>,
    ) -> Result<Json<Value>, String> {
        self.dispatch_input(InputCommand::TypeText { text: args.text })
            .await?;
        Ok(ack_json("text_typed"))
    }

    #[tool(
        description = "Move keyboard focus to an element through its exact semantic focus handler"
    )]
    async fn focus_element(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        self.invoke_node(&tree, &args.id, SemanticAction::Focus)
            .await?;
        Ok(ack_json("focus_requested"))
    }

    #[tool(
        description = "Return annotated text, caret, selection, and redaction metadata for an element"
    )]
    async fn get_text_info(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let text = get_node(&tree, &args.id)?
            .text
            .as_ref()
            .ok_or_else(|| format!("element {:?} has no annotated text", args.id))?;
        Ok(object_output(
            serde_json::to_value(text).map_err(encode_error)?,
        ))
    }

    #[tool(
        description = "Replace text in an annotated editable element through its exact semantic handler"
    )]
    async fn set_text(
        &self,
        Parameters(args): Parameters<SetTextArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let node = get_node(&tree, &args.id)?;
        if node.text.as_ref().is_none_or(|text| text.redacted) {
            return Err("element is not an editable non-secret annotated text field".to_owned());
        }
        if !node.actions.contains(&NodeAction::SetText) {
            return Err(format!(
                "element {:?} does not advertise the set_text action",
                args.id
            ));
        }
        self.invoke_node(&tree, &args.id, SemanticAction::SetText { text: args.text })
            .await?;
        Ok(ack_json("text_replaced"))
    }

    #[tool(description = "Return annotated value metadata including min, max, and step")]
    async fn get_value(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let value = get_node(&tree, &args.id)?
            .value
            .as_ref()
            .ok_or_else(|| format!("element {:?} has no annotated value", args.id))?;
        Ok(object_output(
            serde_json::to_value(value).map_err(encode_error)?,
        ))
    }

    #[tool(
        description = "Set a value-bearing element through its exact handler; annotated numeric bounds are enforced"
    )]
    async fn set_value(
        &self,
        Parameters(args): Parameters<SetValueArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let node = get_node(&tree, &args.id)?;
        let value = node
            .value
            .as_ref()
            .ok_or_else(|| format!("element {:?} has no annotated value", args.id))?;
        validate_value(&args.value, value)?;
        if !node.actions.contains(&NodeAction::SetValue) {
            return Err(format!(
                "element {:?} does not advertise the set_value action",
                args.id
            ));
        }
        self.invoke_node(
            &tree,
            &args.id,
            SemanticAction::SetValue { value: args.value },
        )
        .await?;
        Ok(ack_json("value_set"))
    }

    #[tool(description = "Count elements whose annotated selected state is true")]
    async fn get_selection_count(&self) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let count = tree
            .nodes
            .values()
            .filter(|node| node.state.selected == Some(true))
            .count();
        Ok(object_output(json!({ "selected_count": count })))
    }

    #[tool(
        description = "Return visible, enabled, focused, checked, selected, and expanded state for an element"
    )]
    async fn get_element_state(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<Json<Value>, String> {
        let tree = self.tree().await?;
        let state = &get_node(&tree, &args.id)?.state;
        Ok(object_output(
            serde_json::to_value(state).map_err(encode_error)?,
        ))
    }

    #[tool(
        description = "Invoke an annotated semantic scroll handler by ID or coordinate. This does NOT dispatch a GPUI wheel event; use native_scroll/native_scroll_coordinates for the real native wheel pipeline."
    )]
    async fn scroll(
        &self,
        Parameters(args): Parameters<ScrollArgs>,
    ) -> Result<Json<Value>, String> {
        match (&args.id, args.x, args.y) {
            (Some(id), None, None) => {
                let tree = self.tree().await?;
                self.invoke_node(
                    &tree,
                    id,
                    SemanticAction::Scroll {
                        delta_x: args.delta_x,
                        delta_y: args.delta_y,
                    },
                )
                .await?;
            }
            (None, Some(x), Some(y)) => {
                self.dispatch_input(InputCommand::Scroll {
                    point: Point { x, y },
                    delta_x: args.delta_x,
                    delta_y: args.delta_y,
                })
                .await?;
            }
            _ => return Err("provide either id or both x and y".to_owned()),
        }
        Ok(ack_json("scrolled"))
    }
}

fn validate_pointer_click_count(count: u8) -> Result<(), String> {
    if (1..=3).contains(&count) {
        Ok(())
    } else {
        Err("pointer click count must be between 1 and 3".to_owned())
    }
}

pub(super) fn router() -> ToolRouter<GpuiMcp> {
    GpuiMcp::input_router()
}
