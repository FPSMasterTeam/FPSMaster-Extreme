//! Stable input events for the `on_input` hook.

/// A keyboard/mouse input event projected from the host's winit events. `key` is
/// a stable name (e.g. `"KeyW"`, `"Escape"`, `"Mouse0"`) rather than a raw winit
/// keycode, so the public surface does not couple to winit. A hook returns `true`
/// to consume the event (suppress default gameplay handling).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputEvent {
    pub key: String,
    pub pressed: bool,
}

impl InputEvent {
    pub fn new(key: impl Into<String>, pressed: bool) -> Self {
        Self {
            key: key.into(),
            pressed,
        }
    }
}
