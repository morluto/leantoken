const SERVER_NAME: &str = "leantoken";
const DISCOVERY_SKILL_MARKER: &str = "<!-- managed by leantoken setup -->";

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
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) version: Option<String>,
    pub(crate) expected_version: String,
    pub(crate) matches_current: bool,
}

/// Coding clients supported by the global setup wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupClient {
    /// Claude Code.
    Claude,
    /// Cursor.
    Cursor,
    /// OpenCode.
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

    fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Antigravity => "Antigravity",
        }
    }

    fn definition(self, home: &Path) -> ClientDefinition {
        match self {
            Self::Claude => ClientDefinition::json(
                home.join(".claude.json"),
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
            ),
            Self::Cursor => ClientDefinition::json(
                home.join(".cursor/mcp.json"),
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
            ),
            Self::OpenCode => {
                let directory = home.join(".config/opencode");
                let candidates = [
                    directory.join("opencode.json"),
                    directory.join("opencode.jsonc"),
                    directory.join(".opencode.json"),
                    directory.join(".opencode.jsonc"),
                ];
                let path = candidates
                    .iter()
                    .find(|candidate| candidate.exists())
                    .cloned()
                    .unwrap_or_else(|| candidates[0].clone());
                ClientDefinition::json(path, "mcp", JsonEntryShape::OpenCode)
            }
            Self::Codex => ClientDefinition {
                path: home.join(".codex/config.toml"),
                format: ConfigFormat::Toml,
            },
            Self::Gemini => ClientDefinition::json(
                home.join(".gemini/settings.json"),
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
            ),
            Self::Antigravity => ClientDefinition::json(
                home.join(".gemini/config/mcp_config.json"),
                "mcpServers",
                JsonEntryShape::CommandAndArgs,
            ),
        }
    }

    fn is_detected(self, home: &Path) -> bool {
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
}

#[derive(Debug, Clone)]
struct ClientDefinition {
    path: PathBuf,
    format: ConfigFormat,
}

impl ClientDefinition {
    fn json(path: PathBuf, section: &'static str, shape: JsonEntryShape) -> Self {
        Self {
            path,
            format: ConfigFormat::Json { section, shape },
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ConfigFormat {
    Json {
        section: &'static str,
        shape: JsonEntryShape,
    },
    Toml,
}

#[derive(Debug, Clone, Copy)]
enum JsonEntryShape {
    CommandAndArgs,
    OpenCode,
}
