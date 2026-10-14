#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarPage {
    Profiles,
    History,
    Bookmarks,
    Extensions,
    Downloads,
}

impl SidebarPage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Profiles => "Profiles",
            Self::History => "History",
            Self::Bookmarks => "Bookmarks",
            Self::Extensions => "Extensions",
            Self::Downloads => "Downloads",
        }
    }

    pub fn all() -> [Self; 5] {
        [Self::Profiles, Self::History, Self::Bookmarks, Self::Extensions, Self::Downloads]
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "history" => Self::History,
            "bookmarks" => Self::Bookmarks,
            "extensions" => Self::Extensions,
            "downloads" => Self::Downloads,
            _ => Self::Profiles,
        }
    }
}
