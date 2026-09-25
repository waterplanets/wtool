//! 配置持久化：GUI 与无窗口主机（`--host`）共用同一份配置。
//!
//! 存在应用数据目录下的单文件 `%APPDATA%\wtool\config.json`：机器级、不随
//! 游戏目录分走，GUI 关掉、换成脚本启动也都读得到。
//!
//! 为什么不用 localStorage（原来那样）：脚本/主机模式没有 WebView，读不到；
//! 而“填一次就一直用”正是主机模式的前提。
//!
//! API Key 存这里（明文，落本机）。这是权衡后的选择：主机模式必须能无人值守
//! 地拿到 key，而环境变量对普通用户太重。文件里没填时退级到环境变量
//! `WTOOL_LLM_KEY`，给 CI / 临时场景留一条不进磁盘的路。
//!
//! 不新增 crate：路径自己按 `APPDATA` 环境变量拼，写盘自己写 `.tmp` 再 rename
//! （同 `engine::rgss3::patch::write_atomic` 的做法）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// 一份应用配置。字段与 `task::Config` 对齐，另加两个输出格式。
///
/// 用 `#[serde(default)]`：老配置文件缺字段时按默认补，不因为加了个字段就
/// 让用户重新配一遍。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub game_dir: String,
    pub dict_path: String,
    pub font: String,
    /// `gtx`（Google，免费零配置）或 `llm`（OpenAI 兼容）。
    pub translate_backend: String,
    pub corpus_path: String,
    pub out_path: String,
    /// 语料输出格式：`tsv` 或 `json`。
    pub corpus_format: String,
    /// 词典输出格式：`tsv` 或 `json`。
    pub dict_format: String,
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_api_key: String,
    pub llm_batch: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            game_dir: String::new(),
            dict_path: String::new(),
            font: String::new(),
            translate_backend: "gtx".to_string(),
            corpus_path: String::new(),
            out_path: String::new(),
            corpus_format: "tsv".to_string(),
            dict_format: "tsv".to_string(),
            llm_base_url: "https://api.deepseek.com".to_string(),
            llm_model: "deepseek-chat".to_string(),
            llm_api_key: String::new(),
            llm_batch: 20,
        }
    }
}

impl AppConfig {
    /// 读配置。文件不在、读不动、JSON 坏了都退回默认值，**不报错** ——
    /// 配置只是“上次填过什么”的记忆，坏了不该挡住用户干活。
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 写配置。写同目录的 `.tmp` 再 rename，避免崩在中间留下半个文件。
    pub fn save(&self) -> Result<()> {
        let path = path().ok_or_else(|| Error::new("找不到应用数据目录（APPDATA 未设置）"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;

        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text)?;
        // Windows 上 rename 到已存在的文件会失败，先删目标（`.tmp` 已完整落盘）。
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// 配置里的 API Key，空则退到环境变量 `WTOOL_LLM_KEY`。
    pub fn api_key(&self) -> String {
        let from_file = self.llm_api_key.trim();
        if !from_file.is_empty() {
            return from_file.to_string();
        }
        std::env::var("WTOOL_LLM_KEY")
            .map(|k| k.trim().to_string())
            .unwrap_or_default()
    }

    /// 把用户填的东西收拾成一份能用的配置。
    ///
    /// 前端/命令行填什么进来都有可能：路径带引号、后端名拼错、批大小填 0、
    /// 格式留空。这些一律在这里一次性处理掉，下游（任务层、注入层）就不用
    /// 各自防一遍 —— 和 `config.rs` 只认「干净值」这个分工配套。
    pub fn normalize(&mut self) {
        // 游戏目录收拾到「目录」为止（可能指到了 Game.exe），其余路径只去引号。
        self.game_dir = crate::engine::resolve_game_dir(&self.game_dir)
            .to_string_lossy()
            .into_owned();
        self.dict_path = crate::engine::clean_path(&self.dict_path);
        self.corpus_path = crate::engine::clean_path(&self.corpus_path);
        self.out_path = crate::engine::clean_path(&self.out_path);

        // 后端只认 gtx / llm 两个值，其余（含空串、拼错）一律按默认 gtx。
        if self.translate_backend.trim() != "llm" {
            self.translate_backend = "gtx".to_string();
        }
        if self.corpus_format.trim().is_empty() {
            self.corpus_format = "tsv".to_string();
        }
        if self.dict_format.trim().is_empty() {
            self.dict_format = "tsv".to_string();
        }
        if self.llm_batch == 0 {
            self.llm_batch = 20;
        }
    }
}

/// 配置目录：`%APPDATA%\wtool`。
///
/// 退级顺序：`APPDATA` → `LOCALAPPDATA` → 可执行文件同级目录。前两个在
/// Windows 上总是有，最后那条只是兜底（非 Windows / 环境变量被清掉）。
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)))?;
    Some(base.join("wtool"))
}

/// 配置文件路径：`%APPDATA%\wtool\config.json`。
pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join("config.json"))
}

/// 主机模式（`--host`）的日志文件：`%APPDATA%\wtool\host.log`。
///
/// 和配置同目录，一个地方就能找到全套东西。脚本启动没有终端可看，
/// 日志必须落盘，否则出错时什么都查不到。
pub fn log_path() -> Option<PathBuf> {
    dir().map(|d| d.join("host.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认值就是“零配置可用”：gtx 后端、TSV 输出。
    #[test]
    fn default_is_zero_config() {
        let c = AppConfig::default();
        assert_eq!(c.translate_backend, "gtx");
        assert_eq!(c.corpus_format, "tsv");
        assert_eq!(c.dict_format, "tsv");
        assert!(c.llm_api_key.is_empty());
        assert_eq!(c.llm_batch, 20);
    }

    /// 老配置缺字段时按默认补齐，不整个失败。
    #[test]
    fn missing_fields_fall_back_to_default() {
        let c: AppConfig = serde_json::from_str(r#"{"game_dir":"D:\\g"}"#).unwrap();
        assert_eq!(c.game_dir, "D:\\g");
        assert_eq!(c.translate_backend, "gtx");
        assert_eq!(c.llm_batch, 20);
    }

    /// 配置里有 key 就用它；没有才退环境变量。
    #[test]
    fn api_key_prefers_file_then_env() {
        let mut c = AppConfig::default();
        c.llm_api_key = "sk-file".to_string();
        assert_eq!(c.api_key(), "sk-file");

        // 空 key 时读环境变量。测试里用独有变量名，避免和其它测试打架。
        c.llm_api_key = String::new();
        std::env::set_var("WTOOL_LLM_KEY", "sk-env");
        assert_eq!(c.api_key(), "sk-env");
        std::env::remove_var("WTOOL_LLM_KEY");
    }
}