#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabState {
    Active,
    Background,
    Suspended,
    Closed,
}

impl TabState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Background => "background",
            Self::Suspended => "suspended",
            Self::Closed => "closed",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "active" => Self::Active,
            "background" => Self::Background,
            "closed" => Self::Closed,
            _ => Self::Suspended,
        }
    }

    pub fn keeps_webview(self) -> bool {
        matches!(self, Self::Active | Self::Background)
    }
}

#[derive(Clone, Debug)]
pub struct LiveTab {
    pub id: String,
    pub state: TabState,
    pub background_since: Option<i64>,
}

pub fn suspend_candidates(
    tabs: &[LiveTab],
    now: i64,
    suspend_after_secs: i64,
    max_live: usize,
) -> Vec<String> {
    let mut suspend = Vec::new();
    for tab in tabs {
        if tab.state == TabState::Background {
            if let Some(since) = tab.background_since {
                if now.saturating_sub(since) >= suspend_after_secs {
                    suspend.push(tab.id.clone());
                }
            }
        }
    }
    let mut live: Vec<&LiveTab> = tabs
        .iter()
        .filter(|tab| tab.state.keeps_webview() && !suspend.iter().any(|id| id == &tab.id))
        .collect();
    if live.len() > max_live {
        // No timestamp means backgrounded before this run (restored tabs): those are the oldest.
        live.sort_by_key(|tab| tab.background_since.unwrap_or(i64::MIN));
        let overflow = live.len() - max_live;
        for tab in live.into_iter().filter(|tab| tab.state == TabState::Background).take(overflow)
        {
            if !suspend.iter().any(|id| id == &tab.id) {
                suspend.push(tab.id.clone());
            }
        }
    }
    suspend
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_background_tabs_are_suspended_and_live_count_is_capped() {
        let tabs = vec![
            LiveTab {
                id: "a".into(),
                state: TabState::Active,
                background_since: None,
            },
            LiveTab {
                id: "b".into(),
                state: TabState::Background,
                background_since: Some(0),
            },
            LiveTab {
                id: "c".into(),
                state: TabState::Background,
                background_since: Some(50),
            },
        ];
        let ids = suspend_candidates(&tabs, 120, 100, 2);
        assert!(ids.contains(&"b".to_string()));
        assert!(!ids.contains(&"a".to_string()));
    }

    #[test]
    fn over_the_cap_restored_tabs_go_before_the_tab_just_left() {
        let tab = |id: &str, state, since| LiveTab { id: id.into(), state, background_since: since };
        let tabs = vec![
            tab("active", TabState::Active, None),
            tab("restored", TabState::Background, None),
            tab("just-left", TabState::Background, Some(100)),
        ];
        assert_eq!(suspend_candidates(&tabs, 101, 120, 2), vec!["restored".to_string()]);
    }
}
