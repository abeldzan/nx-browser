//! Coding agents running in a terminal tab and what they are doing, read the way herdr does:
//! the foreground process names which agent it is; its window title and the bottom of the screen say its state.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Cursor,
    Copilot,
    Amp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// At its prompt, nothing happening.
    Idle,
    Working,
    /// Waiting on the user: a permission prompt or a question.
    Blocked,
    /// Finished while its tab was in the background; cleared once the tab is looked at.
    Done,
}

/// The agent in a terminal's foreground and what it is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentState {
    pub agent: Agent,
    pub status: Status,
}

impl Agent {
    pub fn from_process(name: &str) -> Option<Self> {
        Some(match name.rsplit('/').next().unwrap_or(name) {
            "claude" | "claude-code" => Self::Claude,
            "codex" => Self::Codex,
            "opencode" | "open-code" => Self::OpenCode,
            "gemini" => Self::Gemini,
            "cursor-agent" => Self::Cursor,
            "copilot" => Self::Copilot,
            "amp" => Self::Amp,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini",
            Self::Cursor => "Cursor",
            Self::Copilot => "Copilot",
            Self::Amp => "Amp",
        }
    }

    /// The logo's asset path (see `Assets` in lib.rs); Amp has none and gets a letter tile.
    pub fn icon(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("icons/agents/claude.svg"),
            Self::Codex => Some("icons/agents/openai.svg"),
            Self::OpenCode => Some("icons/agents/opencode.svg"),
            Self::Gemini => Some("icons/agents/gemini.svg"),
            Self::Cursor => Some("icons/agents/cursor.svg"),
            Self::Copilot => Some("icons/agents/copilot.svg"),
            Self::Amp => None,
        }
    }

    /// Brand colour for the logo; `None` for monochrome brands, drawn in the text colour.
    pub fn color(self) -> Option<u32> {
        match self {
            Self::Claude => Some(0xD97757),
            Self::Gemini => Some(0x8E75B2),
            Self::Amp => Some(0xF34E3F),
            Self::Codex | Self::OpenCode | Self::Cursor | Self::Copilot => None,
        }
    }
}

/// What the agent is doing, from the title it set and the bottom of its screen.
// shortcut: only the main rules from herdr's distribution/agent-detection manifests; port more there when a state reads wrong.
pub fn status(agent: Agent, title: Option<&str>, tail: &str) -> Status {
    let title = title.unwrap_or_default();
    let tail = tail.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| tail.contains(needle));
    let spinner = title.chars().any(is_spinner);
    let (blocked, working) = match agent {
        Agent::Claude => (
            has(&["do you want to proceed?", "waiting for permission", "do you want to allow"]) || (tail.contains("esc to cancel") && tail.contains("enter to")),
            title.starts_with(|c| is_spinner(c) || ('\u{25D0}'..='\u{25D3}').contains(&c)) || has(&["esc to interrupt"]),
        ),
        Agent::Codex => (
            title.contains("Action Required") || has(&["allow command?", "press enter to confirm or esc to cancel", "[y/n]"]),
            spinner || has(&["to interrupt)"]),
        ),
        Agent::OpenCode => (has(&["△ permission required"]), has(&["esc to interrupt", "esc interrupt", "■■■■", "⬝⬝⬝⬝"])),
        Agent::Gemini | Agent::Cursor | Agent::Copilot | Agent::Amp => (has(&["do you want to", "[y/n]", "allow once", "waiting for user confirmation"]), spinner || has(&["esc to cancel", "esc to interrupt"])),
    };
    match (blocked, working) {
        (true, _) => Status::Blocked,
        (_, true) => Status::Working,
        _ => Status::Idle,
    }
}

/// A braille spinner frame, as agents put in their title while working; the blank cell U+2800 is not one.
fn is_spinner(c: char) -> bool {
    ('\u{2801}'..='\u{28FF}').contains(&c)
}

/// The session's name from the title the agent set, without its status glyph (`⠂ `, `✳ `);
/// `None` when the title only names the agent or a state.
pub fn session_title(agent: Agent, title: &str) -> Option<String> {
    let title = title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    let generic = title.is_empty()
        || title.eq_ignore_ascii_case(agent.name())
        || Agent::from_process(&title.to_lowercase()) == Some(agent)
        || title.contains("Action Required");
    (!generic).then(|| title.to_string())
}

/// The status to show: an agent that stops working while its tab is in the background is Done until the tab is seen.
pub fn next(prev: Option<Status>, now: Status, seen: bool) -> Status {
    match (prev, now) {
        (Some(Status::Working | Status::Done), Status::Idle) if !seen => Status::Done,
        _ => now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_are_found_by_process_name() {
        assert_eq!(Agent::from_process("claude"), Some(Agent::Claude));
        assert_eq!(Agent::from_process("/opt/homebrew/bin/codex"), Some(Agent::Codex));
        assert_eq!(Agent::from_process("opencode"), Some(Agent::OpenCode));
        assert_eq!(Agent::from_process("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(Agent::from_process("vim"), None);
        assert_eq!(Agent::from_process("node"), None);
    }

    #[test]
    fn every_agent_logo_is_bundled() {
        use gpui::AssetSource;
        for agent in [Agent::Claude, Agent::Codex, Agent::OpenCode, Agent::Gemini, Agent::Cursor, Agent::Copilot, Agent::Amp] {
            if let Some(path) = agent.icon() {
                assert!(crate::Assets.load(path).ok().flatten().is_some(), "{agent:?}'s logo {path} is missing from Assets::load in lib.rs");
            }
        }
    }

    #[test]
    fn claude_states() {
        assert_eq!(status(Agent::Claude, Some("⠂ Fix the build"), ""), Status::Working);
        assert_eq!(status(Agent::Claude, Some("✳ Claude Code"), "> \n? for shortcuts"), Status::Idle);
        assert_eq!(status(Agent::Claude, None, "✻ Pondering… (12s · esc to interrupt)"), Status::Working);
        let prompt = "Bash command\n  rm -rf target\nDo you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
        assert_eq!(status(Agent::Claude, Some("⠂ Fix the build"), prompt), Status::Blocked);
    }

    #[test]
    fn codex_and_opencode_states() {
        assert_eq!(status(Agent::Codex, Some("Action Required"), ""), Status::Blocked);
        assert_eq!(status(Agent::Codex, Some("⠹ codex"), ""), Status::Working);
        assert_eq!(status(Agent::Claude, Some("\u{2800} Fix the build"), ""), Status::Idle);
        assert_eq!(status(Agent::Codex, None, "• Working (5s • esc to interrupt)"), Status::Working);
        assert_eq!(status(Agent::Codex, Some("codex"), "› Ask Codex to do anything"), Status::Idle);
        assert_eq!(status(Agent::OpenCode, None, "△ Permission required\n  Bash ls"), Status::Blocked);
        assert_eq!(status(Agent::OpenCode, None, "■■■■⬝⬝⬝⬝  esc interrupt"), Status::Working);
        assert_eq!(status(Agent::OpenCode, None, "ctrl+p commands"), Status::Idle);
    }

    #[test]
    fn session_titles_drop_status_glyphs_and_generic_names() {
        assert_eq!(session_title(Agent::Claude, "⠂ Fix the build").as_deref(), Some("Fix the build"));
        assert_eq!(session_title(Agent::Claude, "✳ Fix the build").as_deref(), Some("Fix the build"));
        assert_eq!(session_title(Agent::Claude, "✳ Claude Code"), None);
        assert_eq!(session_title(Agent::Codex, "⠹ codex"), None);
        assert_eq!(session_title(Agent::Codex, "Action Required"), None);
        assert_eq!(session_title(Agent::OpenCode, "OpenCode"), None);
        assert_eq!(session_title(Agent::OpenCode, "◐ ").as_deref(), None);
    }

    #[test]
    fn finishing_in_the_background_is_done_until_seen() {
        assert_eq!(next(Some(Status::Working), Status::Idle, false), Status::Done);
        assert_eq!(next(Some(Status::Done), Status::Idle, false), Status::Done);
        assert_eq!(next(Some(Status::Done), Status::Idle, true), Status::Idle);
        assert_eq!(next(Some(Status::Working), Status::Idle, true), Status::Idle);
        assert_eq!(next(Some(Status::Done), Status::Working, false), Status::Working);
        assert_eq!(next(None, Status::Idle, false), Status::Idle);
    }
}
