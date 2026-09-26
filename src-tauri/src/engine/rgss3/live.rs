//! RGSS3 的「实时连接」注入 —— 文件轮询式热重载。
//!
//! 和一次性那条（[`super::patch`]）用的是**同一份钩子源码**，差别只有两点：
//!
//! 1. 钩子里 `LIVE = true`，于是启动后会挂上 `Graphics.update`，每秒比一次
//!    词典 / 字体文件的 mtime，变了就地重载；
//! 2. 词典、字体、日志各自写到另一组文件（`wtool_live_*`），互不干扰。
//!
//! 这条路存在的意义就是**调字体不用重装**：wtool 跑着的时候把用户在界面上选的
//! 字体写进 `Patch/wtool_live_font.txt`，游戏侧一秒内就换过来 —— 不用停游戏、
//! 不用重装钩子、不用重算 `Patch.rb` 的认证号。试词典同理。
//!
//! 为什么不走 TCP：RGSS3 的游戏进程是原生 `Game.exe`，没有 NW.js 那套 JS 运行时，
//! 钩子是纯 Ruby、跑在游戏主线程里；给它架一条 socket 通道既重又容易卡帧。写文件
//! 是零依赖的：wtool 只管改文件，钩子只管看 mtime。
//!
//! 代价是这条路**不持久**：wtool 退出即卸（`persistent = false`），因为关了 wtool
//! 就没人再往里写字体了，「实时」也就无从谈起 —— 想装完即终态请用一次性那条。

use std::path::Path;

use crate::dict;
use crate::engine::inject::{Mechanism, Mode, Report, Reversibility, Service, State};
use crate::error::{Error, Result};

use super::patch::{self, HookConfig};

/// 实时那条的 loader 起始标记，和一次性那条不同 —— 两条各自摘各自的。
const MARKER: &str = "# ---- wtool RGSS3 live loader (auto-generated) ----";

/// 钩子源码。和一次性共用同一份（编译期嵌进二进制）。
const HOOK_NAME: &str = "wtool_live.rb";
const DICT_NAME: &str = "wtool_live_dict.tsv";
const FONT_NAME: &str = "wtool_live_font.txt";
const LOG_NAME: &str = "wtool_live.log";
const MISS_NAME: &str = "wtool_live_miss.log";
const ERR_NAME: &str = "wtool_live.err";

/// 「文件轮询式实时」这条注入路。
pub struct Rgss3Live;

impl Mechanism for Rgss3Live {
    fn id(&self) -> &'static str {
        "rgss3_live"
    }

    fn name(&self) -> &'static str {
        "RGSS3 实时（改字体即时生效）"
    }

    fn mode(&self) -> Mode {
        Mode::Live
    }

    /// 不靠 socket，靠 wtool 往游戏目录写文件、钩子轮询 —— 所以仍需 wtool 常驻。
    fn service(&self) -> Service {
        Service::FileSync
    }

    fn probe(&self, game_dir: &Path) -> Report {
        let dir = game_dir.join("Patch");
        if !dir.join("Patch.rb").is_file() {
            return Report {
                state: State::NotApplicable,
                detail: "没有 Patch/Patch.rb，用不了 NWPatch 这条路".to_string(),
            };
        }

        let hook = dir.join(HOOK_NAME);
        match std::fs::read_to_string(&hook) {
            Err(_) => Report {
                state: State::Ready,
                detail: "还没装，可以安装".to_string(),
            },
            Ok(rb) if patch::strip_config_lines(&rb) != patch::strip_config_lines(patch::HOOK_RB) => {
                Report {
                    state: State::Stale,
                    detail: "钩子不是当前版本，需要重装".to_string(),
                }
            }
            Ok(_) => {
                let patch_rb = std::fs::read_to_string(dir.join("Patch.rb")).unwrap_or_default();
                if !patch_rb.contains(MARKER) {
                    Report {
                        state: State::Stale,
                        detail: "钩子在，但 Patch.rb 里没挂 loader，游戏不会加载，需要重装"
                            .to_string(),
                    }
                } else if !dir.join(DICT_NAME).is_file() {
                    Report {
                        state: State::Stale,
                        detail: "词典文件不在，需要重装".to_string(),
                    }
                } else {
                    Report {
                        state: State::Installed,
                        detail: "已安装（实时），保持 wtool 打开，改字体即时生效".to_string(),
                    }
                }
            }
        }
    }

    fn install(&self, game_dir: &Path, dict_path: &Path, font: &str) -> Result<Vec<String>> {
        install(game_dir, dict_path, font)
    }

    fn uninstall(&self, game_dir: &Path) -> Result<Vec<String>> {
        uninstall(game_dir)
    }

    /// 热更新字体：只改那个字体文件，钩子一秒内自己重载 —— 这就是实时模式的意义。
    fn update_font(&self, game_dir: &Path, font: &str) -> Result<Vec<String>> {
        let dir = patch::find_patch_dir(game_dir)?;
        patch::write_atomic(&dir.join(FONT_NAME), &font_blob(font))?;
        let shown = if font.trim().is_empty() {
            "默认（黑体）".to_string()
        } else {
            font.trim().to_string()
        };
        Ok(vec![format!(
            "字体已热更新为「{shown}」，游戏侧一秒内生效（不用重启游戏）"
        )])
    }

    fn reversibility(&self) -> Reversibility {
        Reversibility::RuntimeAndOffline
    }

    fn restart_required(&self) -> bool {
        true
    }

    /// 不持久：wtool 退出时会被 `commands::cleanup_on_exit` 卸掉 —— 关了 wtool
    /// 就没人再写字体文件了，「实时」也就名不副实。
    fn persistent(&self) -> bool {
        false
    }
}

/// 装实时钩子：转词典、写字体文件、写钩子、挂 loader。
fn install(game_dir: &Path, dict_path: &Path, font: &str) -> Result<Vec<String>> {
    let dir = patch::find_patch_dir(game_dir)?;
    let mut log = Vec::new();

    if dict_path.as_os_str().is_empty() {
        return Err(Error::new(
            "RGSS3 这条路要把词典投进游戏目录，请先在界面上选好词典文件",
        ));
    }

    // ---- 1. 词典 TSV ----
    let map = dict::load_map(dict_path)?;
    let (tsv, stats) = patch::build_tsv(&map, dict_path);
    let dict_dst = dir.join(DICT_NAME);
    patch::write_atomic(&dict_dst, &tsv)?;
    log.push(format!(
        "词典 {} 条 → {}（跳过 纯 ASCII {}、单字 {}、空译文 {}、含换行/制表 {}）",
        stats.kept,
        dict_dst.display(),
        stats.ascii,
        stats.short,
        stats.empty,
        stats.ctl,
    ));

    // ---- 2. 字体文件（实时模式下 wtool 靠改这个文件热更新字体）----
    patch::write_atomic(&dir.join(FONT_NAME), &font_blob(font))?;

    // ---- 3. 钩子本体（LIVE=true，路径指向上面这组 wtool_live_* 文件）----
    let cfg = HookConfig {
        live: true,
        dict: format!("Patch/{DICT_NAME}"),
        log: format!("Patch/{LOG_NAME}"),
        miss: format!("Patch/{MISS_NAME}"),
        err: format!("Patch/{ERR_NAME}"),
        font_path: format!("Patch/{FONT_NAME}"),
        // 实时这条的字体以字体文件为准；FONT_NAME 保留默认值当回退
        //（字体文件为空/读不到时仍能画中文）。
        font_name: String::new(),
    };
    let hook_src = patch::build_hook(&cfg)?;
    let hook = dir.join(HOOK_NAME);
    patch::write_atomic(&hook, &hook_src)?;
    log.push(format!("已写入 {}", hook.display()));

    // ---- 4. Patch.rb：挂上实时那条的 loader ----
    patch::attach_loader(
        &dir,
        MARKER,
        &patch::build_loader(MARKER, &format!("Patch/{HOOK_NAME}"), &format!("Patch/{ERR_NAME}")),
        &mut log,
    )?;
    log.push("保持 wtool 打开：改字体、换词典都会在一秒内热更新到游戏里".to_string());
    log.push("游戏如果已经开着，要重启一次才会加载钩子（之后就不用再重启了）".to_string());

    Ok(log)
}

/// 卸实时钩子：摘 loader + 清掉自己那组文件。
fn uninstall(game_dir: &Path) -> Result<Vec<String>> {
    let dir = patch::find_patch_dir(game_dir)?;
    let mut log = Vec::new();

    patch::detach_or_restore(&dir, MARKER, &mut log)?;

    for name in [HOOK_NAME, DICT_NAME, FONT_NAME, LOG_NAME, MISS_NAME, ERR_NAME] {
        patch::remove_file_logged(&dir.join(name), &mut log);
    }

    log.push("游戏如果正开着，要重启才会真的断开".to_string());
    Ok(log)
}

/// 字体文件的正文。空串也写一个换行 —— 钩子读到空白会回退到默认字体，
/// 而 mtime 变化能触发一次重载（不然「清空字体」这个动作传不过去）。
fn font_blob(font: &str) -> String {
    format!("{}\n", font.trim())
}
