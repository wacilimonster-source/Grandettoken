use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Client {
    Trae,
    Workbuddy,
    WorkbuddyAi,
    Zcode,
    Dsh,
    ClaudeCode,
    Codex,
    OpenCode,
}

impl Client {
    pub const ALL: [Client; 8] = [
        Client::Trae,
        Client::Workbuddy,
        Client::WorkbuddyAi,
        Client::Zcode,
        Client::Dsh,
        Client::ClaudeCode,
        Client::Codex,
        Client::OpenCode,
    ];

    /// 界面上的短标签(模型行里那几个小按钮)。
    pub fn short(self) -> &'static str {
        match self {
            Client::Trae => "T",
            Client::Workbuddy => "WB",
            Client::WorkbuddyAi => "WBA",
            Client::Zcode => "ZC",
            Client::Dsh => "DSH",
            Client::ClaudeCode => "CC",
            Client::Codex => "CX",
            Client::OpenCode => "OC",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Client::Trae => "Trae",
            Client::Workbuddy => "WorkBuddy",
            Client::WorkbuddyAi => "WorkBuddy AI",
            Client::Zcode => "ZCode",
            Client::Dsh => "DeepSeek Harness",
            Client::ClaudeCode => "Claude Code",
            Client::Codex => "Codex",
            Client::OpenCode => "OpenCode",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Client::Trae => "trae",
            Client::Workbuddy => "workbuddy",
            Client::WorkbuddyAi => "workbuddy-ai",
            Client::Zcode => "zcode",
            Client::Dsh => "dsh",
            Client::ClaudeCode => "claude-code",
            Client::Codex => "codex",
            Client::OpenCode => "opencode",
        }
    }

    pub fn parse(s: &str) -> Option<Client> {
        Client::ALL.into_iter().find(|c| c.id() == s)
    }

    /// 进程名。两个 WorkBuddy 共用 `WorkBuddy.exe`,所以判「在跑」时**互相牵连**:
    /// 分不清跑的是哪一个,就一律按「有一个在跑」处理(保守方向)。
    pub fn process(self) -> &'static str {
        self.processes()[0]
    }

    /// 判「在跑」要查的全部进程名,任一命中即算在跑。
    /// OpenCode 桌面版把 CLI 装成 `opencode-cli.exe`,只查 `opencode.exe` 会漏判。
    pub fn processes(self) -> &'static [&'static str] {
        match self {
            Client::Trae => &["Trae CN.exe"],
            Client::Workbuddy | Client::WorkbuddyAi => &["WorkBuddy.exe"],
            Client::Zcode => &["ZCode.exe"],
            Client::Dsh => &["DeepSeek Harness.exe"],
            Client::ClaudeCode => &["claude.exe"],
            Client::Codex => &["codex.exe"],
            Client::OpenCode => &["opencode.exe", "opencode-cli.exe", "opencode-pty.exe"],
        }
    }

    /// 「目标在跑就不能写」这条规矩适不适用。
    pub fn blocks_when_running(self) -> bool {
        !matches!(self, Client::Dsh)
    }
}

/// 一个具体的写入目标(= 客户端 × 安装/变体)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Site {
    pub client: Client,
    /// "Trae CN" / "TRAE SOLO CN" / "WorkBuddy" …
    pub label: String,
    pub running: bool,
    pub process: String,
    /// 配置根目录(备份就放在它的 backup-inject/ 下)
    pub root: PathBuf,
    /// 这次要动/要备份的文件
    pub files: Vec<PathBuf>,
}
