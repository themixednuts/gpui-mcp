use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_mcp_protocol::{
    ActionOutcome, BridgeError, ErrorCode, FrameStats, Highlight, InputCommand, LogEntry,
    MAX_ID_BYTES, MAX_LABEL_BYTES, MAX_METADATA_FIELDS, MAX_METADATA_KEY_BYTES,
    MAX_METADATA_VALUE_BYTES, MAX_TEXT_BYTES, MAX_TREE_NODES, NodeAction, Point, Rect,
    SemanticAction, SemanticDiagnostic, SemanticDiagnosticCode, UiNode, UiTree,
};
use tokio::sync::watch;
use tokio::time::timeout;

use crate::element::{NodeEvent, NodeHandler};

const MAX_TIMING_SAMPLES: usize = 240;
const MAX_DIAGNOSTICS: usize = 128;
const MAX_ACTION_REASON_BYTES: usize = 512;

#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowGeometry {
    pub(crate) content_bounds: Rect,
    pub(crate) scale_factor: f32,
}

thread_local! {
    static PARENT_STACK: RefCell<Vec<(u64, String)>> = const { RefCell::new(Vec::new()) };
    static HANDLERS: RefCell<BTreeMap<(u64, String), NodeHandler>> = const { RefCell::new(BTreeMap::new()) };
}

#[derive(Debug, Default)]
struct PendingFrame {
    active: bool,
    nodes: BTreeMap<String, UiNode>,
    order: Vec<String>,
    diagnostics: Vec<SemanticDiagnostic>,
}

#[derive(Debug)]
struct TimingState {
    previous_frame: Option<Instant>,
    prepaint_started: Option<Instant>,
    root_paint_started: Option<Instant>,
    frame_count: u64,
    intervals: VecDeque<Duration>,
    prepaint: VecDeque<Duration>,
    root_paint: VecDeque<Duration>,
}

impl Default for TimingState {
    fn default() -> Self {
        Self {
            previous_frame: None,
            prepaint_started: None,
            root_paint_started: None,
            frame_count: 0,
            intervals: VecDeque::with_capacity(MAX_TIMING_SAMPLES),
            prepaint: VecDeque::with_capacity(MAX_TIMING_SAMPLES),
            root_paint: VecDeque::with_capacity(MAX_TIMING_SAMPLES),
        }
    }
}

#[derive(Debug)]
pub(crate) struct SharedState {
    pub(crate) instance_id: u64,
    tree: RwLock<UiTree>,
    pending: Mutex<PendingFrame>,
    hit_order: RwLock<BTreeMap<String, usize>>,
    highlights: RwLock<Vec<Highlight>>,
    timings: Mutex<TimingState>,
    logs: Mutex<VecDeque<LogEntry>>,
    generation: watch::Sender<u64>,
    completed_frame: watch::Sender<FrameStats>,
    window_geometry: RwLock<Option<WindowGeometry>>,
}

impl SharedState {
    pub(crate) fn new(instance_id: u64) -> Arc<Self> {
        let (generation, _) = watch::channel(0);
        let (completed_frame, _) = watch::channel(FrameStats::default());
        Arc::new(Self {
            instance_id,
            tree: RwLock::new(UiTree::default()),
            pending: Mutex::new(PendingFrame::default()),
            hit_order: RwLock::new(BTreeMap::new()),
            highlights: RwLock::new(Vec::new()),
            timings: Mutex::new(TimingState::default()),
            logs: Mutex::new(VecDeque::with_capacity(512)),
            generation,
            completed_frame,
            window_geometry: RwLock::new(None),
        })
    }

    pub(crate) fn begin_frame(&self) {
        let now = Instant::now();
        {
            let mut timings = self
                .timings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            timings.frame_count = timings.frame_count.saturating_add(1);
            if let Some(previous) = timings.previous_frame.replace(now) {
                push_sample(
                    &mut timings.intervals,
                    now.saturating_duration_since(previous),
                );
            }
            timings.prepaint_started = Some(now);
            timings.root_paint_started = None;
        }

        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous_was_incomplete = pending.active;
        pending.active = true;
        pending.nodes.clear();
        pending.order.clear();
        pending.diagnostics.clear();
        if previous_was_incomplete {
            push_diagnostic(
                &mut pending,
                SemanticDiagnosticCode::InvalidNode,
                None,
                "the previous semantic frame did not reach root paint",
            );
        }
        PARENT_STACK.with(|stack| stack.borrow_mut().retain(|(id, _)| *id != self.instance_id));
        HANDLERS.with(|handlers| {
            handlers
                .borrow_mut()
                .retain(|(instance_id, _), _| *instance_id != self.instance_id);
        });
    }

    pub(crate) fn finish_prepaint(&self) {
        let now = Instant::now();
        let mut timings = self
            .timings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(started) = timings.prepaint_started.take() {
            push_sample(
                &mut timings.prepaint,
                now.saturating_duration_since(started),
            );
        }
    }

    pub(crate) fn begin_root_paint(&self) {
        self.timings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .root_paint_started = Some(Instant::now());
    }

    pub(crate) fn finish_root_paint(&self) {
        let now = Instant::now();
        let stats = {
            let mut timings = self
                .timings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(started) = timings.root_paint_started.take() {
                push_sample(
                    &mut timings.root_paint,
                    now.saturating_duration_since(started),
                );
            }
            frame_stats_from_timings(&timings)
        };
        self.completed_frame.send_replace(stats);
    }

    pub(crate) fn set_window_geometry(&self, content_bounds: Rect, scale_factor: f32) {
        if !content_bounds.is_valid() || !scale_factor.is_finite() || scale_factor <= 0.0 {
            return;
        }
        *self
            .window_geometry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(WindowGeometry {
            content_bounds,
            scale_factor,
        });
    }

    pub(crate) fn window_geometry(&self) -> Option<WindowGeometry> {
        *self
            .window_geometry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn record(&self, mut node: UiNode) -> bool {
        let inferred_parent = PARENT_STACK.with(|stack| {
            stack
                .borrow()
                .iter()
                .rev()
                .find(|(id, _)| *id == self.instance_id)
                .map(|(_, node_id)| node_id.clone())
        });
        if node.parent.is_none() {
            node.parent = inferred_parent;
        }
        node.children.clear();

        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !pending.active {
            return false;
        }
        if let Err(message) = validate_node(&node) {
            let node_id = valid_diagnostic_id(&node.id).then(|| node.id.clone());
            push_diagnostic(
                &mut pending,
                SemanticDiagnosticCode::InvalidNode,
                node_id,
                message,
            );
            return false;
        }
        if pending.nodes.contains_key(&node.id) {
            push_diagnostic(
                &mut pending,
                SemanticDiagnosticCode::DuplicateId,
                Some(node.id),
                "duplicate semantic node identifier was omitted",
            );
            return false;
        }
        if pending.nodes.len() >= MAX_TREE_NODES {
            push_diagnostic(
                &mut pending,
                SemanticDiagnosticCode::CapacityExceeded,
                None,
                "semantic tree capacity was exceeded",
            );
            return false;
        }
        pending.order.push(node.id.clone());
        pending.nodes.insert(node.id.clone(), node);
        true
    }

    pub(crate) fn push_parent(&self, id: &str) {
        PARENT_STACK.with(|stack| {
            stack.borrow_mut().push((self.instance_id, id.to_owned()));
        });
    }

    pub(crate) fn record_handler(&self, node_id: String, handler: NodeHandler) {
        HANDLERS.with(|handlers| {
            handlers
                .borrow_mut()
                .insert((self.instance_id, node_id), handler);
        });
    }

    pub(crate) fn pop_parent(&self) {
        PARENT_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if let Some(index) = stack.iter().rposition(|(id, _)| *id == self.instance_id) {
                stack.remove(index);
            }
        });
    }

    pub(crate) fn finish_frame(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !pending.active {
            return;
        }
        pending.active = false;

        break_parent_cycles(&mut pending);
        let roots = build_relationships(&mut pending);
        let nodes = std::mem::take(&mut pending.nodes);
        let order = std::mem::take(&mut pending.order);
        let diagnostics = std::mem::take(&mut pending.diagnostics);
        drop(pending);

        let mut tree = self
            .tree
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = tree.roots != roots || tree.nodes != nodes || tree.diagnostics != diagnostics;
        if changed {
            tree.generation = tree.generation.saturating_add(1);
        }
        tree.roots = roots;
        tree.nodes = nodes;
        tree.diagnostics = diagnostics;
        let generation = tree.generation;
        drop(tree);

        *self
            .hit_order
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = order
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();
        if changed {
            self.generation.send_replace(generation);
        }
    }

    pub(crate) fn tree(&self) -> UiTree {
        self.tree
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn tree_generation(&self) -> u64 {
        self.tree
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
    }

    pub(crate) async fn wait_for_tree(
        &self,
        after_generation: u64,
        wait: Duration,
    ) -> Result<UiTree, BridgeError> {
        let current = self.tree();
        if current.generation > after_generation {
            return Ok(current);
        }

        let mut receiver = self.generation.subscribe();
        let changed = async {
            loop {
                if *receiver.borrow_and_update() > after_generation {
                    return Ok(());
                }
                receiver.changed().await.map_err(|_| {
                    BridgeError::new(ErrorCode::Internal, "semantic tree publisher stopped")
                })?;
            }
        };
        timeout(wait, changed)
            .await
            .map_err(|_| BridgeError::new(ErrorCode::Timeout, "semantic tree wait timed out"))??;
        Ok(self.tree())
    }

    pub(crate) async fn wait_for_frame(
        &self,
        after_frame_count: u64,
        wait: Duration,
    ) -> Result<FrameStats, BridgeError> {
        let mut receiver = self.completed_frame.subscribe();
        let current = receiver.borrow_and_update().clone();
        if current.frame_count > after_frame_count {
            return Ok(current);
        }

        let changed = async {
            loop {
                receiver.changed().await.map_err(|_| {
                    BridgeError::new(ErrorCode::Internal, "frame publisher stopped")
                })?;
                let current = receiver.borrow_and_update().clone();
                if current.frame_count > after_frame_count {
                    return Ok(current);
                }
            }
        };
        timeout(wait, changed)
            .await
            .map_err(|_| BridgeError::new(ErrorCode::Timeout, "frame wait timed out"))?
    }

    pub(crate) fn dispatch_action(
        &self,
        node_id: &str,
        expected_generation: u64,
        action: &SemanticAction,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Result<ActionOutcome, BridgeError> {
        let tree = self
            .tree
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tree.generation != expected_generation {
            return Err(BridgeError::new(
                ErrorCode::StaleGeneration,
                "semantic tree changed after the element was selected",
            ));
        }
        let node = tree
            .nodes
            .get(node_id)
            .ok_or_else(|| BridgeError::new(ErrorCode::NotFound, "semantic node was not found"))?;
        if !node.state.visible || !node.state.enabled {
            return Err(BridgeError::new(
                ErrorCode::Rejected,
                "semantic node is not visible and enabled",
            ));
        }
        let required = action.required_node_action();
        if !node.actions.contains(&required) {
            return Err(BridgeError::new(
                ErrorCode::Unsupported,
                "semantic node does not advertise the requested action",
            ));
        }
        let bounds = node.bounds;
        drop(tree);

        let handler = HANDLERS.with(|handlers| {
            handlers
                .borrow()
                .get(&(self.instance_id, node_id.to_owned()))
                .cloned()
        });
        let Some(handler) = handler else {
            return Err(BridgeError::new(
                ErrorCode::NotFound,
                "semantic action handler is not available in the current frame",
            ));
        };
        let event = semantic_event(action, bounds)?;
        dispatch_drag_moves(&event, &handler, window, cx);
        Ok(sanitize_outcome(handler(&event, window, cx)))
    }

    pub(crate) fn dispatch_pointer(
        &self,
        command: &InputCommand,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Result<ActionOutcome, BridgeError> {
        let (point, event, required) = coordinate_event(command)?;
        let tree = self.tree();
        let order = self
            .hit_order
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let node_id = tree
            .nodes
            .values()
            .filter(|node| node.state.visible && node.state.enabled)
            .filter(|node| node.actions.contains(&required))
            .filter_map(|node| node.bounds.map(|bounds| (node, bounds)))
            .filter(|(_, bounds)| bounds.contains(point))
            .filter(|(node, _)| {
                HANDLERS.with(|handlers| {
                    handlers
                        .borrow()
                        .contains_key(&(self.instance_id, node.id.clone()))
                })
            })
            .max_by_key(|(node, _)| order.get(&node.id).copied().unwrap_or_default())
            .map(|(node, _)| node.id.clone())
            .ok_or_else(|| {
                BridgeError::new(
                    ErrorCode::Unsupported,
                    "no enabled annotated handler accepts that coordinate action",
                )
            })?;
        drop(order);
        let handler =
            HANDLERS.with(|handlers| handlers.borrow().get(&(self.instance_id, node_id)).cloned());
        let Some(handler) = handler else {
            return Err(BridgeError::new(
                ErrorCode::NotFound,
                "annotated action handler expired before dispatch",
            ));
        };
        dispatch_drag_moves(&event, &handler, window, cx);
        Ok(sanitize_outcome(handler(&event, window, cx)))
    }

    pub(crate) fn set_highlights(&self, highlights: Vec<Highlight>) {
        *self
            .highlights
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = highlights;
    }

    pub(crate) fn highlights(&self) -> Vec<Highlight> {
        self.highlights
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn frame_stats(&self) -> FrameStats {
        self.completed_frame.borrow().clone()
    }

    pub(crate) fn add_log(&self, level: &str, message: &str) {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let mut sanitized = message.replace(['\r', '\n'], " ");
        sanitized.truncate(sanitized.floor_char_boundary(4096));
        let mut logs = self
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if logs.len() == 512 {
            logs.pop_front();
        }
        logs.push_back(LogEntry {
            timestamp_ms,
            level: normalize_level(level).to_owned(),
            message: sanitized,
        });
    }

    pub(crate) fn logs(&self, limit: u16, min_level: Option<&str>) -> Vec<LogEntry> {
        let threshold = min_level.map_or(0, level_rank);
        let take = usize::from(limit.min(512));
        self.logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .rev()
            .filter(|entry| level_rank(&entry.level) >= threshold)
            .take(take)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub(crate) fn clear_logs(&self) {
        self.logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

fn validate_node(node: &UiNode) -> Result<(), &'static str> {
    validate_id(&node.id)?;
    if let Some(parent) = &node.parent {
        validate_id(parent)?;
    }
    for text in [node.label.as_deref(), node.description.as_deref()]
        .into_iter()
        .flatten()
    {
        if text.len() > MAX_LABEL_BYTES || text.chars().any(char::is_control) {
            return Err("semantic label or description is invalid or exceeds 4 KiB");
        }
    }
    if node.bounds.is_some_and(|bounds| {
        !bounds.is_valid()
            || bounds.x.abs() > 1_000_000.0
            || bounds.y.abs() > 1_000_000.0
            || bounds.width > 1_000_000.0
            || bounds.height > 1_000_000.0
    }) {
        return Err("semantic bounds are invalid or exceed the coordinate limit");
    }
    if node
        .actions
        .iter()
        .enumerate()
        .any(|(index, action)| node.actions[..index].contains(action))
    {
        return Err("semantic actions contain a duplicate");
    }
    if let Some(text) = &node.text {
        if text.text.len() > MAX_TEXT_BYTES {
            return Err("semantic text exceeds 64 KiB");
        }
        if text.redacted && !text.text.is_empty() {
            return Err("redacted semantic text must not contain a value");
        }
        if text
            .caret
            .is_some_and(|caret| caret > text.text.len() || !text.text.is_char_boundary(caret))
        {
            return Err("semantic text caret is not a valid UTF-8 boundary");
        }
        if text.selection.is_some_and(|range| {
            range.start > range.end
                || range.end > text.text.len()
                || !text.text.is_char_boundary(range.start)
                || !text.text.is_char_boundary(range.end)
        }) {
            return Err("semantic text selection is not a valid UTF-8 range");
        }
    }
    if let Some(value) = &node.value {
        if value.value.len() > MAX_TEXT_BYTES {
            return Err("semantic value exceeds 64 KiB");
        }
        if [value.min, value.max, value.step]
            .into_iter()
            .flatten()
            .any(|number| !number.is_finite())
        {
            return Err("semantic numeric bounds must be finite");
        }
        if value.min.zip(value.max).is_some_and(|(min, max)| min > max) {
            return Err("semantic numeric minimum exceeds its maximum");
        }
        if value.step.is_some_and(|step| step <= 0.0) {
            return Err("semantic numeric step must be positive");
        }
    }
    if node.metadata.len() > MAX_METADATA_FIELDS {
        return Err("semantic metadata exceeds 32 fields");
    }
    if node.metadata.iter().any(|(key, value)| {
        key.is_empty()
            || key.len() > MAX_METADATA_KEY_BYTES
            || key.chars().any(char::is_control)
            || value.len() > MAX_METADATA_VALUE_BYTES
            || value.chars().any(char::is_control)
    }) {
        return Err("semantic metadata contains an invalid or oversized field");
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() || id.len() > MAX_ID_BYTES || id.chars().any(char::is_control) {
        return Err("semantic identifier must contain 1-256 bytes without control characters");
    }
    Ok(())
}

fn valid_diagnostic_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES && !id.chars().any(char::is_control)
}

fn push_diagnostic(
    pending: &mut PendingFrame,
    code: SemanticDiagnosticCode,
    node_id: Option<String>,
    message: &'static str,
) {
    if pending.diagnostics.len() < MAX_DIAGNOSTICS {
        pending.diagnostics.push(SemanticDiagnostic {
            code,
            node_id,
            message: message.to_owned(),
        });
    }
}

fn break_parent_cycles(pending: &mut PendingFrame) {
    for start in pending.order.clone() {
        let mut seen = BTreeSet::new();
        let mut current = start.clone();
        loop {
            if !seen.insert(current.clone()) {
                if let Some(node) = pending.nodes.get_mut(&start) {
                    node.parent = None;
                }
                push_diagnostic(
                    pending,
                    SemanticDiagnosticCode::ParentCycle,
                    Some(start),
                    "semantic parent cycle was broken at this node",
                );
                break;
            }
            let Some(parent) = pending
                .nodes
                .get(&current)
                .and_then(|node| node.parent.clone())
            else {
                break;
            };
            if !pending.nodes.contains_key(&parent) {
                break;
            }
            current = parent;
        }
    }
}

fn build_relationships(pending: &mut PendingFrame) -> Vec<String> {
    let relationships: Vec<_> = pending
        .order
        .iter()
        .filter_map(|id| {
            pending
                .nodes
                .get(id)
                .map(|node| (id.clone(), node.parent.clone()))
        })
        .collect();
    let mut roots = Vec::new();
    for (child, parent) in relationships {
        if let Some(parent) = parent {
            if let Some(parent_node) = pending.nodes.get_mut(&parent) {
                parent_node.children.push(child);
            } else {
                if let Some(node) = pending.nodes.get_mut(&child) {
                    node.parent = None;
                }
                push_diagnostic(
                    pending,
                    SemanticDiagnosticCode::MissingParent,
                    Some(child.clone()),
                    "semantic parent was not present; node was promoted to a root",
                );
                roots.push(child);
            }
        } else {
            roots.push(child);
        }
    }
    roots
}

/// Deliver interpolated [`NodeEvent::DragMove`] events preceding a semantic drag's
/// final [`NodeEvent::Drag`], one per step in `1..steps`. A no-op for non-drag events
/// and when `steps <= 1`, so single-step drags remain fully backward compatible.
fn dispatch_drag_moves(
    event: &NodeEvent,
    handler: &NodeHandler,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let NodeEvent::Drag { from, to, steps } = *event else {
        return;
    };
    for i in 1..steps {
        let progress = f32::from(i) / f32::from(steps);
        let at = interpolate(from, to, progress);
        handler(&NodeEvent::DragMove { at, progress }, window, cx);
    }
}

/// Linearly interpolate between two points by `fraction`, typically `0.0..=1.0`.
fn interpolate(from: Point, to: Point, fraction: f32) -> Point {
    Point {
        x: from.x + (to.x - from.x) * fraction,
        y: from.y + (to.y - from.y) * fraction,
    }
}

fn semantic_event(action: &SemanticAction, bounds: Option<Rect>) -> Result<NodeEvent, BridgeError> {
    let center = || {
        bounds.map(Rect::center).ok_or_else(|| {
            BridgeError::new(ErrorCode::Rejected, "semantic node has no current bounds")
        })
    };
    match action {
        SemanticAction::Click { button, count } => Ok(NodeEvent::Click {
            button: *button,
            count: *count,
        }),
        SemanticAction::Focus => Ok(NodeEvent::Focus),
        SemanticAction::Hover => Ok(NodeEvent::Hover { point: center()? }),
        SemanticAction::Drag { to, steps } => Ok(NodeEvent::Drag {
            from: center()?,
            to: *to,
            steps: *steps,
        }),
        SemanticAction::Scroll { delta_x, delta_y } => Ok(NodeEvent::Scroll {
            delta_x: *delta_x,
            delta_y: *delta_y,
        }),
        SemanticAction::SetText { text } => Ok(NodeEvent::SetText { text: text.clone() }),
        SemanticAction::SetValue { value } => Ok(NodeEvent::SetValue {
            value: value.clone(),
        }),
    }
}

fn coordinate_event(
    command: &InputCommand,
) -> Result<(gpui_mcp_protocol::Point, NodeEvent, NodeAction), BridgeError> {
    match command {
        InputCommand::Click {
            point,
            button,
            count,
        } => Ok((
            *point,
            NodeEvent::Click {
                button: *button,
                count: *count,
            },
            NodeAction::Click,
        )),
        InputCommand::Hover { point } => Ok((
            *point,
            NodeEvent::Hover { point: *point },
            NodeAction::Hover,
        )),
        InputCommand::Drag { from, to, steps } => Ok((
            *from,
            NodeEvent::Drag {
                from: *from,
                to: *to,
                steps: *steps,
            },
            NodeAction::Drag,
        )),
        InputCommand::Scroll {
            point,
            delta_x,
            delta_y,
        } => Ok((
            *point,
            NodeEvent::Scroll {
                delta_x: *delta_x,
                delta_y: *delta_y,
            },
            NodeAction::Scroll,
        )),
        InputCommand::Key { .. }
        | InputCommand::TypeText { .. }
        | InputCommand::ReplaceText { .. } => Err(BridgeError::new(
            ErrorCode::Internal,
            "keyboard input was routed as coordinate input",
        )),
    }
}

fn sanitize_outcome(outcome: ActionOutcome) -> ActionOutcome {
    match outcome {
        ActionOutcome::Handled => ActionOutcome::Handled,
        ActionOutcome::Rejected { reason } => {
            let mut reason = reason.replace(['\r', '\n'], " ");
            reason.truncate(reason.floor_char_boundary(MAX_ACTION_REASON_BYTES));
            if reason.is_empty() {
                "application rejected the semantic action".clone_into(&mut reason);
            }
            ActionOutcome::Rejected { reason }
        }
    }
}

fn push_sample(samples: &mut VecDeque<Duration>, sample: Duration) {
    if samples.len() == MAX_TIMING_SAMPLES {
        samples.pop_front();
    }
    samples.push_back(sample);
}

fn frame_stats_from_timings(timings: &TimingState) -> FrameStats {
    let (frame_interval_average_ms, frame_interval_max_ms) = timing_summary(&timings.intervals);
    let (prepaint_average_ms, prepaint_max_ms) = timing_summary(&timings.prepaint);
    let (root_paint_average_ms, root_paint_max_ms) = timing_summary(&timings.root_paint);
    FrameStats {
        frame_count: timings.frame_count,
        sample_count: u32::try_from(timings.intervals.len()).unwrap_or(u32::MAX),
        frame_interval_average_ms,
        frame_interval_max_ms,
        prepaint_average_ms,
        prepaint_max_ms,
        root_paint_average_ms,
        root_paint_max_ms,
        estimated_fps: if frame_interval_average_ms > 0.0 {
            1000.0 / frame_interval_average_ms
        } else {
            0.0
        },
    }
}

#[allow(clippy::cast_precision_loss)]
fn timing_summary(samples: &VecDeque<Duration>) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let total_ms = samples.iter().map(Duration::as_secs_f64).sum::<f64>() * 1000.0;
    let average_ms = total_ms / samples.len() as f64;
    let max_ms = samples
        .iter()
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .fold(0.0, f64::max);
    (average_ms, max_ms)
}

fn normalize_level(level: &str) -> &'static str {
    match level.to_ascii_lowercase().as_str() {
        "trace" => "trace",
        "debug" => "debug",
        "warn" | "warning" => "warn",
        "error" => "error",
        _ => "info",
    }
}

fn level_rank(level: &str) -> u8 {
    match normalize_level(level) {
        "trace" => 0,
        "debug" => 1,
        "warn" => 3,
        "error" => 4,
        _ => 2,
    }
}

pub(crate) fn rect_from_gpui(bounds: gpui::Bounds<gpui::Pixels>) -> Rect {
    Rect {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use gpui_mcp_protocol::{NodeState, Point, Role, SemanticDiagnosticCode, UiNode};

    use super::{SharedState, interpolate};

    fn node(id: &str, parent: Option<&str>) -> UiNode {
        UiNode {
            id: id.to_owned(),
            parent: parent.map(str::to_owned),
            children: Vec::new(),
            role: Role::Generic,
            label: None,
            description: None,
            bounds: None,
            state: NodeState::default(),
            actions: Vec::new(),
            text: None,
            value: None,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn interpolate_reaches_both_endpoints_and_the_midpoint() {
        let from = Point { x: 10.0, y: 20.0 };
        let to = Point { x: 30.0, y: 60.0 };

        assert_eq!(interpolate(from, to, 0.0), from);
        assert_eq!(interpolate(from, to, 1.0), to);
        assert_eq!(interpolate(from, to, 0.5), Point { x: 20.0, y: 40.0 });
    }

    #[test]
    fn duplicate_ids_are_omitted_and_reported() {
        let state = SharedState::new(1);
        state.begin_frame();
        assert!(state.record(node("same", None)));
        assert!(!state.record(node("same", None)));
        state.finish_frame();

        let tree = state.tree();
        assert_eq!(tree.nodes.len(), 1);
        assert!(tree.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == SemanticDiagnosticCode::DuplicateId
                && diagnostic.node_id.as_deref() == Some("same")
        }));
    }

    #[test]
    fn missing_parents_and_cycles_are_repaired_deterministically() {
        let state = SharedState::new(2);
        state.begin_frame();
        assert!(state.record(node("missing", Some("absent"))));
        assert!(state.record(node("a", Some("b"))));
        assert!(state.record(node("b", Some("a"))));
        state.finish_frame();

        let tree = state.tree();
        assert!(tree.roots.contains(&"missing".to_owned()));
        assert!(tree.nodes["missing"].parent.is_none());
        assert!(
            tree.diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == SemanticDiagnosticCode::MissingParent })
        );
        assert!(
            tree.diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == SemanticDiagnosticCode::ParentCycle })
        );
        assert!(tree.roots.iter().any(|root| root == "a" || root == "b"));
    }

    #[test]
    fn unchanged_semantics_do_not_advance_generation() {
        let state = SharedState::new(3);
        for _ in 0..2 {
            state.begin_frame();
            assert!(state.record(node("stable", None)));
            state.finish_frame();
        }
        assert_eq!(state.tree().generation, 1);
    }

    #[tokio::test]
    async fn tree_wait_wakes_when_a_new_generation_is_published() -> Result<(), String> {
        let state = SharedState::new(4);
        let waiter_state = state.clone();
        let waiter =
            tokio::spawn(
                async move { waiter_state.wait_for_tree(0, Duration::from_secs(1)).await },
            );
        tokio::task::yield_now().await;

        state.begin_frame();
        assert!(state.record(node("published", None)));
        state.finish_frame();

        let tree = waiter
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.message)?;
        assert_eq!(tree.generation, 1);
        assert!(tree.nodes.contains_key("published"));
        Ok(())
    }

    #[tokio::test]
    async fn frame_wait_wakes_after_unchanged_semantics_finish_paint() -> Result<(), String> {
        let state = SharedState::new(5);
        state.begin_frame();
        assert!(state.record(node("stable", None)));
        state.finish_frame();
        state.begin_root_paint();
        state.finish_root_paint();
        assert_eq!(state.tree().generation, 1);

        let waiter_state = state.clone();
        let waiter =
            tokio::spawn(
                async move { waiter_state.wait_for_frame(1, Duration::from_secs(1)).await },
            );
        tokio::task::yield_now().await;

        state.begin_frame();
        assert!(state.record(node("stable", None)));
        state.finish_frame();
        state.begin_root_paint();
        state.finish_root_paint();

        let observed_frame = waiter
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.message)?;
        assert_eq!(observed_frame.frame_count, 2);
        assert_eq!(state.tree().generation, 1);
        Ok(())
    }

    #[tokio::test]
    async fn frame_wait_never_returns_a_started_but_incomplete_frame() -> Result<(), String> {
        let state = SharedState::new(6);
        state.begin_frame();
        assert!(state.record(node("stable", None)));
        state.finish_frame();
        state.begin_root_paint();
        state.finish_root_paint();

        state.begin_frame();
        assert!(state.record(node("stable", None)));
        state.finish_frame();
        state.begin_root_paint();
        assert_eq!(state.frame_stats().frame_count, 1);

        let wait = state.wait_for_frame(1, Duration::from_secs(1));
        tokio::pin!(wait);
        tokio::select! {
            biased;
            result = &mut wait => {
                return Err(format!(
                    "frame wait completed before root paint: {:?}",
                    result.map_err(|error| error.message)
                ));
            }
            () = tokio::task::yield_now() => {}
        }

        state.finish_root_paint();
        let observed = wait.await.map_err(|error| error.message)?;
        assert_eq!(observed.frame_count, 2);
        assert_eq!(state.frame_stats(), observed);
        Ok(())
    }
}
