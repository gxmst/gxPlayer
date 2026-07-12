use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

pub const TRANSPORT_EVENT: &str = "gx-media";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportAction {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
}

impl TransportAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Toggle => "toggle",
            Self::Next => "next",
            Self::Previous => "previous",
        }
    }
}

impl TryFrom<&str> for TransportAction {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "play" => Ok(Self::Play),
            "pause" => Ok(Self::Pause),
            "toggle" => Ok(Self::Toggle),
            "next" => Ok(Self::Next),
            "previous" => Ok(Self::Previous),
            _ => Err(format!("unknown media action: {value}")),
        }
    }
}

pub fn dispatch(app: &AppHandle, action: TransportAction) -> Result<(), String> {
    app.emit(TRANSPORT_EVENT, action.as_str())
        .map_err(|error| format!("emit {TRANSPORT_EVENT}: {error}"))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportCapabilities {
    pub revision: u64,
    pub has_current: bool,
    pub can_previous: bool,
    pub can_next: bool,
}

#[derive(Debug, Default)]
pub struct TransportState {
    capabilities: Mutex<TransportCapabilities>,
}

impl TransportState {
    pub fn snapshot(&self) -> TransportCapabilities {
        self.capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_capabilities(&self, mut capabilities: TransportCapabilities) {
        let mut current = self
            .capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.has_current == capabilities.has_current
            && current.can_previous == capabilities.can_previous
            && current.can_next == capabilities.can_next
        {
            return;
        }
        capabilities.revision = current
            .revision
            .wrapping_add(1)
            .max(capabilities.revision)
            .max(1);
        *current = capabilities;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_wire_names_are_stable() {
        for (wire, action) in [
            ("play", TransportAction::Play),
            ("pause", TransportAction::Pause),
            ("toggle", TransportAction::Toggle),
            ("next", TransportAction::Next),
            ("previous", TransportAction::Previous),
        ] {
            assert_eq!(action.as_str(), wire);
            assert_eq!(TransportAction::try_from(wire), Ok(action));
        }
        assert!(TransportAction::try_from("stop").is_err());
    }

    #[test]
    fn capability_revision_only_changes_with_behavior() {
        let state = TransportState::default();
        state.set_capabilities(TransportCapabilities {
            revision: 4,
            has_current: true,
            can_previous: false,
            can_next: true,
        });
        assert_eq!(state.snapshot().revision, 4);

        state.set_capabilities(TransportCapabilities {
            revision: 99,
            has_current: true,
            can_previous: false,
            can_next: true,
        });
        assert_eq!(state.snapshot().revision, 4);

        state.set_capabilities(TransportCapabilities {
            revision: 1,
            has_current: true,
            can_previous: true,
            can_next: true,
        });
        let snapshot = state.snapshot();
        assert_eq!(snapshot.revision, 5);
        assert!(snapshot.can_previous);
    }
}
