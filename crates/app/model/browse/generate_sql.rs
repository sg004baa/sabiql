#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerateSqlKind {
    Select,
    Insert,
    Update,
    Delete,
}

impl GenerateSqlKind {
    pub const ALL: [Self; 4] = [Self::Select, Self::Insert, Self::Update, Self::Delete];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}
