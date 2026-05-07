#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusedPane {
    #[default]
    Explorer,
    Inspector,
    Result,
}

impl FocusedPane {
    pub fn from_browse_key(key: char) -> Option<Self> {
        match key {
            '1' => Some(Self::Explorer),
            '2' => Some(Self::Inspector),
            '3' => Some(Self::Result),
            _ => None,
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Explorer => Self::Inspector,
            Self::Inspector => Self::Result,
            Self::Result => Self::Explorer,
        }
    }

    #[must_use]
    pub fn prev(self) -> Self {
        match self {
            Self::Explorer => Self::Result,
            Self::Inspector => Self::Explorer,
            Self::Result => Self::Inspector,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn default_is_explorer() {
        assert_eq!(FocusedPane::default(), FocusedPane::Explorer);
    }

    #[rstest]
    #[case('1', FocusedPane::Explorer)]
    #[case('2', FocusedPane::Inspector)]
    #[case('3', FocusedPane::Result)]
    fn from_browse_key_returns_correct_pane(#[case] key: char, #[case] expected: FocusedPane) {
        assert_eq!(FocusedPane::from_browse_key(key), Some(expected));
    }

    #[rstest]
    #[case('4')]
    #[case('0')]
    #[case('a')]
    fn from_browse_key_returns_none_for_invalid(#[case] key: char) {
        assert_eq!(FocusedPane::from_browse_key(key), None);
    }

    #[rstest]
    #[case(FocusedPane::Explorer, FocusedPane::Inspector)]
    #[case(FocusedPane::Inspector, FocusedPane::Result)]
    #[case(FocusedPane::Result, FocusedPane::Explorer)]
    fn next_cycles_forward(#[case] from: FocusedPane, #[case] expected: FocusedPane) {
        assert_eq!(from.next(), expected);
    }

    #[rstest]
    #[case(FocusedPane::Explorer, FocusedPane::Result)]
    #[case(FocusedPane::Inspector, FocusedPane::Explorer)]
    #[case(FocusedPane::Result, FocusedPane::Inspector)]
    fn prev_cycles_backward(#[case] from: FocusedPane, #[case] expected: FocusedPane) {
        assert_eq!(from.prev(), expected);
    }
}
