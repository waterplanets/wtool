//! 前端与 Rust 之间的一层薄封装。
//!
//! 三件事：把 `invoke` 的名字和参数收在一处（改后端只动这里）、把 Rust 侧那些
//! 结构体写成 TS 接口（字段名保持 snake_case，和 serde 的字段一一对上）、
//! 把错误统一成一句能给用户看的话。
//!
//! 注意参数命名的两层规则：**命令参数顶端用 camelCase**（Tauri 自动转成
//! Rust 的 snake_case 形参），**结构体内部字段用 snake_case**（serde 直接按
//! 字段名解，不做转换）。所以 `invoke("collect_offline", { gameDir, out })`，
//! 而 `invoke("set_config", { cfg: { game_dir, ... } })`。

import { invoke } from "@tauri-apps/api/core";

/** 与 Rust 侧 `config::AppConfig` 一一对应。 */
export interface AppConfig {
  game_dir: string;
  dict_path: string;
  font: string;
  /** `gtx`（Google，免费零配置）或 `llm`（OpenAI 兼容）。 */
  translate_backend: string;
  corpus_path: string;
  out_path: string;
  /** 语料输出格式：`tsv` 或 `json`。 */
  corpus_format: string;
  /** 词典输出格式：`tsv` 或 `json`。 */
  dict_format: string;
  llm_base_url: string;
  llm_model: string;
  llm_api_key: string;
  llm_batch: number;
}

/** 与 Rust 侧 `engine::inject::MethodStatus` 一一对应。 */
export interface MethodStatus {
  id: string;
  name: string;
  state: "not_applicable" | "ready" | "installed" | "stale";
  detail: string;
  reversible:
    | "runtime_full"
    | "runtime_and_offline"
    | "offline_only"
    | "none";
  restart_required: boolean;
  /** 翻译要不要 wtool 常驻服务。RGSS3 为 false：装完即可关掉 wtool。 */
  needs_server: boolean;
  /** 安装会不会留在游戏目录（退出也不还原）。RGSS3 为 true。 */
  persistent: boolean;
}

/** 与 Rust 侧 `commands::EngineInfo` 一一对应。 */
export interface EngineInfo {
  name: string;
  runtime: string;
  known: boolean;
}

/** 与 Rust 侧 `commands::StartOutcome` 一一对应。 */
export interface StartOutcome {
  /** true = 界面得停在「正在翻译」，别关 wtool。 */
  server_running: boolean;
  persistent: boolean;
  method: string;
}

/** 与 Rust 侧 `model::TsSentence` 一一对应。 */
export interface TsSentence {
  source: string;
  target: string;
}

/** 空配置：字段与 `AppConfig::default()` 一致，界面开屏先用它。 */
export function emptyConfig(): AppConfig {
  return {
    game_dir: "",
    dict_path: "",
    font: "",
    translate_backend: "gtx",
    corpus_path: "",
    out_path: "",
    corpus_format: "tsv",
    dict_format: "tsv",
    llm_base_url: "https://api.deepseek.com",
    llm_model: "deepseek-chat",
    llm_api_key: "",
    llm_batch: 20,
  };
}

/** 读配置（%APPDATA%\wtool\config.json）。读不到就是一份默认配置，不报错。 */
export const loadConfig = () => invoke<AppConfig>("load_config");

/** 存配置。返回后端收拾过的那份（引号会被去掉），界面要跟着刷新。 */
export const setConfig = (cfg: AppConfig) =>
  invoke<AppConfig>("set_config", { cfg });

/** 猜引擎。`known=false` 时界面该提示「检查游戏目录」。 */
export const detectEngine = (gameDir: string) =>
  invoke<EngineInfo>("detect_engine", { gameDir });

/** 这个游戏上支持哪些注入方式、各自什么状态。 */
export const listInjections = (gameDir: string) =>
  invoke<MethodStatus[]>("list_injections", { gameDir });

/** 统一翻译入口。返回的 `server_running` 决定界面接下来怎么提示。 */
export const startTranslation = () => invoke<StartOutcome>("start_translation");

/** 在线采集（要服务，游戏侧推文本上来）。 */
export const startCollect = () => invoke<StartOutcome>("start_collect");

/** 离线翻译：语料 → 词典。 */
export const startTranslateFile = () => invoke<void>("start_translate_file");

/** 停掉后台任务（三个任务一套语义）。 */
export const stopTrans = () => invoke<boolean>("stop_trans");

/** 离线扫描：不开游戏，直接扫目录收语料。 */
export const collectOffline = (gameDir: string, out: string, format: string) =>
  invoke<string>("collect_offline", { gameDir, out, format });

/** 语料另存为 / 换格式。 */
export const convertCorpus = (src: string, dst: string, format: string) =>
  invoke<string>("convert_corpus", { src, dst, format });

/** 词典另存为 / 换格式（输入可以是只读的 MTool 词典）。 */
export const convertDict = (src: string, dst: string, format: string) =>
  invoke<string>("convert_dict", { src, dst, format });

/** 读一份词典文件，拿词条列表。 */
export const loadDictionary = (path: string) =>
  invoke<TsSentence[]>("load_dictionary", { path });

/** 手动装某个注入方式（不含翻译任务）。 */
export const installInjection = (
  gameDir: string,
  id: string,
  dictPath: string,
  font: string,
) => invoke<string[]>("install_injection", { gameDir, id, dictPath, font });

/** 撤销某个注入方式。 */
export const uninstallInjection = (gameDir: string, id: string) =>
  invoke<string[]>("uninstall_injection", { gameDir, id });

/** 写一个常驻主机启动脚本到游戏目录。 */
export const generateLauncher = (gameDir: string) =>
  invoke<string>("generate_launcher", { gameDir });

/** 删掉游戏目录里的常驻主机启动脚本。 */
export const removeLauncher = (gameDir: string) =>
  invoke<string>("remove_launcher", { gameDir });

/** 本机已装字体清单。 */
export const listSystemFonts = () => invoke<string[]>("list_system_fonts");

/** 把后端抛回来的东西压成一句人话。 */
export function messageOf(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}