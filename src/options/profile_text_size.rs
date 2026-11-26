use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The profile's unscaled terminal text size in device-independent pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct ProfileTextSize(u8);

impl ProfileTextSize {
    pub const MIN: u8 = 6;
    pub const MAX: u8 = 72;

    pub fn as_f32(self) -> f32 {
        f32::from(self.0)
    }
}

impl TryFrom<u8> for ProfileTextSize {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        ensure!(
            (Self::MIN..=Self::MAX).contains(&value),
            "Enter a whole text size from {} to {}",
            Self::MIN,
            Self::MAX
        );
        Ok(Self(value))
    }
}

impl From<ProfileTextSize> for u8 {
    fn from(size: ProfileTextSize) -> Self {
        size.0
    }
}

impl fmt::Display for ProfileTextSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn saved_size_requires_a_whole_number_in_range() {
        for value in [json!(5), json!(73), json!(256), json!(12.5), json!("16")] {
            assert!(serde_json::from_value::<ProfileTextSize>(value).is_err());
        }
    }
}
