use std::fmt;

use serde::{Deserialize, Serialize};

/// A value whose `Debug` representation never exposes the wrapped data.
///
/// Serialization remains transparent so storage and protocol boundaries must
/// access sensitive values deliberately without changing their wire shape.
#[derive(Clone, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sensitive<T>(T);

impl<T> Sensitive<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    pub const fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> From<T> for Sensitive<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> fmt::Debug for Sensitive<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::Sensitive;

    #[test]
    fn debug_never_requires_or_renders_inner_debug_output() {
        struct DebugWouldLeak;

        let value = Sensitive::new(DebugWouldLeak);
        assert_eq!(format!("{value:?}"), "[REDACTED]");
    }

    #[test]
    fn serialization_is_transparent() {
        let value = Sensitive::new("protocol-secret".to_owned());
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            "\"protocol-secret\""
        );
        assert_eq!(
            serde_json::from_str::<Sensitive<String>>("\"protocol-secret\"")
                .unwrap()
                .expose(),
            "protocol-secret"
        );
    }
}
