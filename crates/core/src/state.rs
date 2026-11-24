use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssistantState {
    #[default]
    Idle,
    Listening,
    Thinking,
    Speaking,
}

impl AssistantState {
    /// Wire name used on D-Bus.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Listening => "listening",
            Self::Thinking => "thinking",
            Self::Speaking => "speaking",
        }
    }
}

impl fmt::Display for AssistantState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
