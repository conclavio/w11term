use serde::{Deserialize, Serialize};

/// A named marker displayed in the tab strip for a profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TabColor {
    #[default]
    None,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl TabColor {
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
    ];

    pub fn is_default(&self) -> bool {
        *self == Self::None
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Blue => "Blue",
            Self::Purple => "Purple",
        }
    }

    pub fn marker(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Red => Some("🔴"),
            Self::Orange => Some("🟠"),
            Self::Yellow => Some("🟡"),
            Self::Green => Some("🟢"),
            Self::Blue => Some("🔵"),
            Self::Purple => Some("🟣"),
        }
    }
}
