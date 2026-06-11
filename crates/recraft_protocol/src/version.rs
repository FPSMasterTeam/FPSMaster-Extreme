#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolVersion {
    V1_8_9,
}

impl ProtocolVersion {
    pub const fn number(self) -> i32 {
        match self {
            Self::V1_8_9 => 47,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::V1_8_9 => "1.8.9",
        }
    }

    pub const fn from_number(number: i32) -> Option<Self> {
        match number {
            47 => Some(Self::V1_8_9),
            _ => None,
        }
    }
}

pub trait ProtocolSpec {
    const VERSION: ProtocolVersion;
}

#[derive(Debug, Clone, Copy)]
pub struct V1_8_9;

impl ProtocolSpec for V1_8_9 {
    const VERSION: ProtocolVersion = ProtocolVersion::V1_8_9;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_47_selects_1_8_9() {
        assert_eq!(ProtocolVersion::from_number(47), Some(ProtocolVersion::V1_8_9));
        assert_eq!(ProtocolVersion::V1_8_9.number(), 47);
    }
}
