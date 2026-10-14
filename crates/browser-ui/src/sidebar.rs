#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarPage {
    Profiles,
    History,
    Bookmarks,
    Extensions,
}

impl SidebarPage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Profiles => "Profiles",
            Self::History => "History",
            Self::Bookmarks => "Bookmarks",
            Self::Extensions => "Extensions",
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::Profiles, Self::History, Self::Bookmarks, Self::Extensions]
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "history" => Self::History,
            "bookmarks" => Self::Bookmarks,
            "extensions" => Self::Extensions,
            _ => Self::Profiles,
        }
    }
}
