#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarPage {
    Profiles,
    History,
    Bookmarks,
}

impl SidebarPage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Profiles => "Profiles",
            Self::History => "History",
            Self::Bookmarks => "Bookmarks",
        }
    }

    pub fn all() -> [Self; 3] {
        [Self::Profiles, Self::History, Self::Bookmarks]
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "history" => Self::History,
            "bookmarks" => Self::Bookmarks,
            _ => Self::Profiles,
        }
    }
}
