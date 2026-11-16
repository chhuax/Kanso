//! AI 命令行 agent 的种类。

use serde::{Deserialize, Serialize};

/// 已知的 AI 命令行 agent。
///
/// 这里只定义种类本身；从命令行识别 agent 是 kanso-term 的职责，不在本 crate 做。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AgentKind {
    /// Claude Code。
    Claude,
    /// OpenAI Codex CLI。
    Codex,
    /// Gemini CLI。
    Gemini,
    /// Aider。
    Aider,
    /// Cursor Agent。
    CursorAgent,
    /// GitHub Copilot CLI。
    Copilot,
    /// Amp。
    Amp,
    /// OpenCode。
    OpenCode,
    /// Crush。
    Crush,
    /// Goose。
    Goose,
    /// Qwen Code。
    Qwen,
    /// Factory Droid。
    Droid,
    /// OpenHands。
    OpenHands,
    /// Grok CLI。
    Grok,
}

impl AgentKind {
    /// 全部种类，顺序固定。
    pub const ALL: &'static [AgentKind] = &[
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::Aider,
        AgentKind::CursorAgent,
        AgentKind::Copilot,
        AgentKind::Amp,
        AgentKind::OpenCode,
        AgentKind::Crush,
        AgentKind::Goose,
        AgentKind::Qwen,
        AgentKind::Droid,
        AgentKind::OpenHands,
        AgentKind::Grok,
    ];

    /// 稳定的小写 kebab 标识，供 UI 查图标等资源。已发布的取值不得改动。
    pub const fn id(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::Aider => "aider",
            AgentKind::CursorAgent => "cursor-agent",
            AgentKind::Copilot => "copilot",
            AgentKind::Amp => "amp",
            AgentKind::OpenCode => "opencode",
            AgentKind::Crush => "crush",
            AgentKind::Goose => "goose",
            AgentKind::Qwen => "qwen",
            AgentKind::Droid => "droid",
            AgentKind::OpenHands => "openhands",
            AgentKind::Grok => "grok",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_kebab() {
        let ids: HashSet<_> = AgentKind::ALL.iter().map(|a| a.id()).collect();
        assert_eq!(ids.len(), AgentKind::ALL.len());
        assert!(
            ids.iter()
                .all(|i| i.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'))
        );
        assert_eq!(AgentKind::CursorAgent.id(), "cursor-agent");
    }
}
