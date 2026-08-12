use super::*;

pub(super) const SERVER_NAME: &str = "leantoken";
pub(super) const DISCOVERY_SKILL_MARKER: &str = "<!-- managed by leantoken setup -->";
pub(crate) const CODEX_STARTUP_TIMEOUT_SECONDS: u64 = 30;

#[derive(Debug)]
pub(crate) struct SetupDiagnostic {
    pub(crate) registration_status: &'static str,
    pub(crate) configured_clients: Vec<SetupClient>,
    pub(crate) registrations: Vec<ConfiguredRegistration>,
    pub(crate) discovery_status: &'static str,
    pub(crate) discovery_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredRegistration {
    pub(crate) client: SetupClient,
    pub(crate) path: PathBuf,
    pub(crate) source_hash: [u8; 32],
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) startup_timeout_seconds: Option<u64>,
    pub(crate) version: Option<String>,
    pub(crate) expected_version: String,
    pub(crate) matches_current: bool,
    pub(crate) managed: bool,
    pub(crate) enabled: bool,
}

/// Coding clients supported by the global setup wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SetupClient {
    /// Claude Code.
    Claude,
    /// Cursor.
    Cursor,
    /// OpenCode.
    #[value(name = "opencode")]
    OpenCode,
    /// Codex CLI, desktop, and IDE integrations.
    Codex,
    /// Gemini CLI.
    Gemini,
    /// Google Antigravity.
    Antigravity,
}

impl SetupClient {
    /// Every supported client in display order.
    pub const ALL: [Self; 6] = [
        Self::Claude,
        Self::Cursor,
        Self::OpenCode,
        Self::Codex,
        Self::Gemini,
        Self::Antigravity,
    ];

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Antigravity => "Antigravity",
        }
    }

    pub(crate) fn cli_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
        }
    }

    pub(super) fn definition(self, home: &Path) -> ClientDefinition {
        let path = match self {
            Self::OpenCode => {
                let candidates = self.configuration_paths(home);
                candidates
                    .iter()
                    .find(|candidate| candidate.exists())
                    .cloned()
                    .unwrap_or_else(|| candidates[0].clone())
            }
            _ => self.configuration_paths(home)[0].clone(),
        };
        self.definition_at(path)
    }

    pub(super) fn definition_at(self, path: PathBuf) -> ClientDefinition {
        match self {
            Self::Claude => {
                ClientDefinition::json(path, "mcpServers", JsonEntryShape::CommandAndArgs)
            }
            Self::Cursor => {
                ClientDefinition::json(path, "mcpServers", JsonEntryShape::CommandAndArgs)
            }
            Self::OpenCode => ClientDefinition::json(path, "mcp", JsonEntryShape::OpenCode),
            Self::Codex => ClientDefinition {
                path,
                format: ConfigFormat::Toml,
            },
            Self::Gemini => {
                ClientDefinition::json(path, "mcpServers", JsonEntryShape::CommandAndArgs)
            }
            Self::Antigravity => {
                ClientDefinition::json(path, "mcpServers", JsonEntryShape::CommandAndArgs)
            }
        }
    }

    pub(super) fn configuration_paths(self, home: &Path) -> Vec<PathBuf> {
        match self {
            Self::Claude => vec![home.join(".claude.json")],
            Self::Cursor => vec![home.join(".cursor/mcp.json")],
            Self::OpenCode => {
                let directory = home.join(".config/opencode");
                vec![
                    directory.join("opencode.json"),
                    directory.join("opencode.jsonc"),
                    directory.join(".opencode.json"),
                    directory.join(".opencode.jsonc"),
                ]
            }
            Self::Codex => vec![home.join(".codex/config.toml")],
            Self::Gemini => vec![home.join(".gemini/settings.json")],
            Self::Antigravity => vec![home.join(".gemini/config/mcp_config.json")],
        }
    }

    pub(super) fn is_detected(self, home: &Path) -> bool {
        match self {
            Self::Claude => home.join(".claude").exists() || home.join(".claude.json").exists(),
            Self::Cursor => home.join(".cursor").exists(),
            Self::OpenCode => home.join(".config/opencode").exists(),
            Self::Codex => home.join(".codex").exists(),
            Self::Gemini => home.join(".gemini").exists(),
            Self::Antigravity => {
                home.join(".gemini/antigravity").exists() || home.join(".agent").exists()
            }
        }
    }

    pub(super) fn discovery_path(self, home: &Path) -> PathBuf {
        match self {
            Self::Claude => home.join(".claude/skills/leantoken/SKILL.md"),
            Self::Cursor | Self::OpenCode | Self::Codex | Self::Gemini | Self::Antigravity => {
                home.join(".agents/skills/leantoken/SKILL.md")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ClientDefinition {
    pub(super) path: PathBuf,
    pub(super) format: ConfigFormat,
}

impl ClientDefinition {
    pub(super) fn json(path: PathBuf, section: &'static str, shape: JsonEntryShape) -> Self {
        Self {
            path,
            format: ConfigFormat::Json { section, shape },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ConfigFormat {
    Json {
        section: &'static str,
        shape: JsonEntryShape,
    },
    Toml,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum JsonEntryShape {
    CommandAndArgs,
    OpenCode,
}
