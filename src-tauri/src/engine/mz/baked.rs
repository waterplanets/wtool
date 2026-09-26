//! 把词典与字体**烤进** JS 插件 —— MV/MZ 的一次性注入方式。
//!
//! 和实时那条（[`super::bridge`]）写的是同一个文件 `js/plugins/wtool_bridge.js`，
//! 区别只在正文：
//!
//! - 实时：装的是插件源码原文，词典和字体都在运行时经 wtool 的内存通道下发；
//! - 一次性：把词典对象字面量和字体名填进源码里那两行占位，插件自己在本进程
//!   内查表，**完全不连 wtool**，wtool 关掉也照翻。
//!
//! 两条路互斥（同一个引擎上同时装会重叠挂钩），由
//! [`crate::engine::inject::uninstall_siblings`] 在装之前把另一条卸掉。
//!
//! 一次性这条是给「正式玩」用的：装完即终态，不必每次开游戏都先开 wtool。
//! 代价是词典整份塞进插件文件（几十万条就是几十兆），启动解析慢一点 ——
//! 所以默认（`run_mode = auto`）MV/MZ 走实时，只有用户明确选一次性时才走这条。

use std::collections::BTreeMap;
use std::path::Path;

use crate::dict;
use crate::engine::inject::{Mechanism, Mode, Report, Reversibility, Service};
use crate::engine::mz::bridge::{
    self, BAKED_DICT_LINE, BAKED_FONT_LINE, BAKED_PREFIX, BRIDGE_JS,
};
use crate::error::{Error, Result};

/// MV/MZ 的一次性注入路。
pub struct JsPluginBaked;

impl Mechanism for JsPluginBaked {
    fn id(&self) -> &'static str {
        "js_plugin_baked"
    }

    fn name(&self) -> &'static str {
        "JS 插件（一次性，词典烤入）"
    }

    fn mode(&self) -> Mode {
        Mode::OneTime
    }

    /// 词典烤在插件里、游戏自己查表，不需要 wtool 起任何服务。
    fn service(&self) -> Service {
        Service::None
    }

    fn probe(&self, game_dir: &Path) -> Report {
        // 「当前版本」= 盘上那份带着烤入载荷。至于载荷是不是这一份词典、
        // 字体对不对，磁盘上看不出来 —— 那些只有在装的时候才说得清。
        bridge::probe_js(game_dir, |js| js.contains(BAKED_PREFIX))
    }

    /// 词典和字体都烤进插件，所以这两个参数这里都要用上。
    fn install(&self, game_dir: &Path, dict_path: &Path, font: &str) -> Result<Vec<String>> {
        install(game_dir, dict_path, font)
    }

    fn uninstall(&self, game_dir: &Path) -> Result<Vec<String>> {
        // 同一个文件、同一段 plugins.js，卸载动作和实时那条一模一样。
        bridge::uninstall(game_dir)
    }

    /// 盘上动了两个文件，但都能靠备份还回去。
    fn reversibility(&self) -> Reversibility {
        Reversibility::RuntimeAndOffline
    }

    /// `plugins.js` 是启动时读的，改了得重启游戏才生效。
    fn restart_required(&self) -> bool {
        true
    }

    /// 装完就留在游戏目录：wtool 退出时不还原，游戏照常翻译。
    fn persistent(&self) -> bool {
        true
    }
}

/// 装一次性插件：读词典 → 过滤 → 烤进源码 → 落盘 + 注册。
///
/// 词典必须给：这条路的全部意义就是把词典烤进去，没词典就没什么可装的。
fn install(game_dir: &Path, dict_path: &Path, font: &str) -> Result<Vec<String>> {
    if dict_path.as_os_str().is_empty() {
        return Err(Error::new(
            "一次性注入要把词典烤进插件，请先在界面上选好词典文件",
        ));
    }

    let map = dict::load_map(dict_path)?;
    let (baked, kept, skipped) = filter_map(&map);
    if kept == 0 {
        return Err(Error::new(format!(
            "{} 里没有可用的词条（{kept} 条留下，{skipped} 条被跳过），换个词典试试",
            dict_path.display()
        )));
    }

    let json = serde_json::to_string(&baked)?;
    let src = bake_source(&json, font)?;

    let mut log = bridge::install_with(game_dir, &src)?;
    log.push(format!(
        "词典已烤入：{} 条（跳过 纯 ASCII / 单字 / 空译文 {} 条）",
        kept, skipped
    ));
    if !font.trim().is_empty() {
        log.push(format!("字体已烤入：{}（游戏重启后生效）", font.trim()));
    }
    Ok(log)
}

/// 把词典和字体填进插件源码的两行占位。
///
/// 占位行必须**各出现一次且逐字节一致**：替换是纯字符串操作。源码里那两行
/// 被人手改过（多空格、换引号）就会在这里报错，而不是悄悄烤进一份坏插件。
fn bake_source(json: &str, font: &str) -> Result<String> {
    let count = |needle: &str| BRIDGE_JS.matches(needle).count();
    if count(BAKED_DICT_LINE) != 1 || count(BAKED_FONT_LINE) != 1 {
        return Err(Error::new(
            "插件源码里的烤入占位行不见了或被改过，没法烤词典；重新构建一份 wtool 再试",
        ));
    }

    // 字体名里的引号和反斜杠要转义，否则能把整份 JS 拆坏、游戏起不来。
    let escaped = font.trim().replace('\\', "\\\\").replace('"', "\\\"");
    let font_line = format!("    var WTOOL_BAKED_FONT = \"{escaped}\";");
    let dict_line = format!("    var WTOOL_BAKED = {json};");

    Ok(BRIDGE_JS
        .replacen(BAKED_FONT_LINE, &font_line, 1)
        .replacen(BAKED_DICT_LINE, &dict_line, 1))
}

/// 挑出值得烤进去的条目，返回（筛过的表，留下几条，跳过几条）。
///
/// 跳过这些的理由和 RGSS3 烤 TSV 那套一致（见 `rgss3::patch::build_tsv`）：
/// 纯 ASCII 的 key 游戏侧根本不会去查（插件有「含假名/汉字」守卫），单字 key
/// 是文本错位的成因，空译文没有意义 —— 留着只是白占体积、白拖慢启动。
///
/// 和那边唯一的差别：这里**不**因为译文里带换行/制表就跳过 —— 装的是 JSON
/// 对象字面量，控制字符会被转义成 `\n` 这类两字符序列，塞得进去。
fn filter_map(map: &BTreeMap<String, String>) -> (BTreeMap<String, String>, usize, usize) {
    let mut out = BTreeMap::new();
    let mut skipped = 0usize;
    for (k, v) in map {
        if k.is_empty() || v.is_empty() || !has_kana_or_kanji(k) || k.chars().count() < 2 {
            skipped += 1;
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    let kept = out.len();
    (out, kept, skipped)
}

/// 含假名或汉字才算「可能要翻」。和插件侧的守卫、RGSS3 钩子的 `NEEDS_TR`
/// 保持一致的范围。
fn has_kana_or_kanji(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c,
            '\u{3040}'..='\u{30FF}'   // 平假名 + 片假名
            | '\u{3400}'..='\u{4DBF}' // CJK 扩展 A
            | '\u{4E00}'..='\u{9FFF}' // CJK 基本区
            | '\u{F900}'..='\u{FAFF}' // CJK 兼容表意
            | '\u{3005}'              // 々
            | '\u{3007}'              // 〇
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bake_replaces_both_placeholders_once() {
        let src = bake_source("{\"あ\":\"啊\"}", "微软雅黑").unwrap();
        assert!(src.contains("    var WTOOL_BAKED = {\"あ\":\"啊\"};"));
        assert!(src.contains("    var WTOOL_BAKED_FONT = \"微软雅黑\";"));
        // 占位行不该再剩下。
        assert!(!src.contains(BAKED_DICT_LINE));
        assert!(!src.contains(BAKED_FONT_LINE));
        // 实时那份源码里没有烤入标记，于是「盘上这份是不是一次性」判得出来。
        assert!(!BRIDGE_JS.contains(BAKED_PREFIX));
    }

    #[test]
    fn font_quotes_are_escaped() {
        let src = bake_source("{}", "a\"b\\c").unwrap();
        assert!(src.contains(r#"    var WTOOL_BAKED_FONT = "a\"b\\c";"#));
    }

    #[test]
    fn filter_drops_ascii_single_char_and_empty() {
        let mut map = BTreeMap::new();
        map.insert("石化能力".to_string(), "石化能力".to_string());
        map.insert("HP".to_string(), "生命".to_string()); // 纯 ASCII key
        map.insert("あ".to_string(), "啊".to_string()); // 单字 key
        map.insert("たべる".to_string(), String::new()); // 空译文
        let (out, kept, skipped) = filter_map(&map);
        assert_eq!(kept, 1);
        assert_eq!(skipped, 3);
        assert!(out.contains_key("石化能力"));
    }

    #[test]
    fn empty_dict_is_rejected() {
        let dir = std::env::temp_dir().join(format!(
            "wtool_baked_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("js").join("plugins")).unwrap();
        let dict = dir.join("dict.json");
        std::fs::write(&dict, "{}").unwrap();

        let e = install(&dir, &dict, "").unwrap_err();
        assert!(e.message.contains("没有可用的词条"), "{}", e.message);
    }
}
