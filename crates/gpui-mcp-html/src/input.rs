use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Window, div,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::render::dispatch_semantic_event;
use crate::{Binding, ElementId, HookRegistry};

/// Initialize native controls used by the live HTML renderer.
///
/// Call this once for each GPUI [`App`] before constructing windows that render
/// [`crate::LiveHtml`] documents. The initializer installs cross-platform input
/// actions and the local control theme; it does not start network services.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
}

pub(crate) struct RuntimeTextInput {
    input: Entity<InputState>,
    element_id: ElementId,
    bindings: Vec<Binding>,
    hooks: HookRegistry,
    placeholder: String,
    multiline: bool,
    masked: bool,
    disabled: bool,
    document_revision: u64,
    suppressed_programmatic_value: Rc<RefCell<Option<String>>>,
    _subscription: Subscription,
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

impl RuntimeTextInput {
    pub(crate) fn new(
        options: RuntimeTextInputOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = options.placeholder.clone();
        let suppressed_programmatic_value = Rc::new(RefCell::new(None::<String>));
        let suppressed_change = suppressed_programmatic_value.clone();
        let input = cx.new(|cx| {
            let input = InputState::new(window, cx)
                .default_value(options.value)
                .placeholder(placeholder)
                .multi_line(options.multiline);
            if options.masked && !options.multiline {
                input.masked(true)
            } else {
                input
            }
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                if consume_programmatic_echo(&suppressed_change, &value) {
                    return;
                }
                let _ = dispatch_semantic_event(
                    &this.hooks,
                    &this.element_id,
                    &this.bindings,
                    &gpui_mcp::NodeEvent::SetValue { value },
                    window,
                    cx,
                );
            },
        );
        Self {
            input,
            element_id: options.element_id,
            bindings: options.bindings,
            hooks: options.hooks,
            placeholder: options.placeholder,
            multiline: options.multiline,
            masked: options.masked,
            disabled: options.disabled,
            document_revision: options.document_revision,
            suppressed_programmatic_value,
            _subscription: subscription,
        }
    }

    pub(crate) fn sync(
        &mut self,
        options: RuntimeTextInputOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = options.value.as_str();
        let placeholder = options.placeholder.as_str();
        let current_value = self.input.read(cx).value();
        let placeholder_changed = self.placeholder != placeholder;
        if current_value.as_ref() != value || placeholder_changed {
            let suppressed_programmatic_value = self.suppressed_programmatic_value.clone();
            self.input.update(cx, |input, cx| {
                if input.value().as_ref() != value {
                    suppressed_programmatic_value
                        .borrow_mut()
                        .replace(value.to_owned());
                    input.set_value(value.to_owned(), window, cx);
                }
                if placeholder_changed {
                    input.set_placeholder(placeholder.to_owned(), window, cx);
                }
            });
        }
        self.element_id = options.element_id;
        self.bindings = options.bindings;
        self.hooks = options.hooks;
        options.placeholder.clone_into(&mut self.placeholder);
        let disabled_changed = self.disabled != options.disabled;
        self.disabled = options.disabled;
        self.document_revision = options.document_revision;
        if disabled_changed {
            cx.notify();
        }
    }

    pub(crate) const fn is_compatible(&self, multiline: bool, masked: bool) -> bool {
        self.multiline == multiline && self.masked == masked
    }

    pub(crate) fn needs_sync(
        &self,
        document_revision: u64,
        value: &str,
        placeholder: &str,
        disabled: bool,
        cx: &App,
    ) -> bool {
        self.document_revision != document_revision
            || self.input.read(cx).value().as_ref() != value
            || self.placeholder != placeholder
            || self.disabled != disabled
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

fn consume_programmatic_echo(suppressed: &RefCell<Option<String>>, value: &str) -> bool {
    let mut suppressed = suppressed.borrow_mut();
    let matches = suppressed.as_deref() == Some(value);
    suppressed.take();
    matches
}

impl Render for RuntimeTextInput {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let input = Input::new(&self.input)
            .appearance(false)
            .bordered(false)
            .focus_bordered(false)
            .disabled(self.disabled)
            .p_0()
            .w_full();
        let input = if self.multiline {
            input.h_full()
        } else {
            input
        };
        div().size_full().child(input)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::consume_programmatic_echo;

    #[test]
    fn programmatic_value_echo_is_consumed_once_without_masking_user_changes() {
        let suppressed = RefCell::new(Some("measured".to_owned()));
        assert!(consume_programmatic_echo(&suppressed, "measured"));
        assert!(!consume_programmatic_echo(&suppressed, "measured"));

        suppressed.replace(Some("old".to_owned()));
        assert!(!consume_programmatic_echo(&suppressed, "user edit"));
        assert!(suppressed.borrow().is_none());
    }
}
