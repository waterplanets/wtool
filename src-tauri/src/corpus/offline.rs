//! 离线采集：不开游戏，直接扫游戏目录里的 `data/*.json` 和 `js/plugins.js`，
//! 把「待翻译的原文」收进语料。
//!
//! 这是复刻桥（`wtool_bridge.js`）里那几段「开机整批问过去」的收集逻辑
//! （DB_TABLES / SYS_ARRAYS / 事件 101·102·401·405 / displayName），加两处桥里
//! 不一定碰得到的：
//!
//! - **插件参数**（`js/plugins.js`）：像 SceneCustomMenu 那种自定义菜单把菜单项
//!   文字写在插件参数里（`Scene1 → "\I[188]手帳"`），桥只在画面真画到那串字时
//!   才收到，离线扫描直接把这些参数一次全收进来，不用把游戏玩到那个菜单。
//! - **独特数据**（`data/unique/`）：桥要等 `$DU` 载入才有，离线直接读文件。
//!
//! 收完只出一个 [`Corpus`] 和计数，**不调任何翻译接口** —— 这一步的目的就是
//! 攒料 + 汇报「收了多少、去重后还剩多少」，供第 3 步喂 LLM 前核对。

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::corpus::plugins::{extract_config, extract_string_literals, Extractor};
use crate::corpus::{Corpus, Granularity};
use crate::error::Result;

/// 一张数据库表：在哪个 `data/*.json`、数组在哪个键下、翻哪几个字段。
/// 字段表和桥里的 `DB_TABLES` 一致（description 只有物品/技能/武器/防具有，
/// message1..4 只有技能/状态有，nickname/profile 只有角色有）。
struct Table {
    file: &'static str,
    key: &'static str,
    fields: &'static [&'static str],
}

const DB_TABLES: &[Table] = &[
    Table { file: "Items.json", key: "items", fields: &["name", "description"] },
    Table { file: "Skills.json", key: "skills", fields: &["name", "description", "message1", "message2"] },
    Table { file: "Weapons.json", key: "weapons", fields: &["name", "description"] },
    Table { file: "Armors.json", key: "armors", fields: &["name", "description"] },
    Table { file: "States.json", key: "states", fields: &["name", "message1", "message2", "message3", "message4"] },
    Table { file: "Actors.json", key: "actors", fields: &["name", "nickname", "profile"] },
    Table { file: "Classes.json", key: "classes", fields: &["name"] },
    Table { file: "Enemies.json", key: "enemies", fields: &["name"] },
    Table { file: "Troops.json", key: "troops", fields: &["name"] },
    Table { file: "Tilesets.json", key: "tilesets", fields: &["name"] },
    Table { file: "Animations.json", key: "animations", fields: &["name"] },
    Table { file: "CommonEvents.json", key: "commonEvents", fields: &["name"] },
    Table { file: "MapInfos.json", key: "infos", fields: &["name"] },
];

/// `$dataSystem` 那组名字数组（技能/装备类型、存档标题…会显示在游戏里）。
const SYS_ARRAYS: &[&str] = &[
    "switches",
    "variables",
    "skillTypes",
    "equipTypes",
    "armorTypes",
    "weaponTypes",
];

/// 独特数据里不翻的键 —— 翻了游戏就找不到分支了（和桥的 `SD_SKIP` 一致）。
const SD_SKIP: &[&str] = &[
    "monsterType",
    "SPAction",
    "criticalAction",
    "action",
    "actionNo",
    "requestPleasureAction",
];

/// 扫一个游戏目录，收成一份语料。
pub fn scan(game_dir: &Path) -> Result<Corpus> {
    let data = data_dir(game_dir)
        .ok_or_else(|| crate::error::Error::new("找不到游戏目录里的 data/ 文件夹，目录是不是填错了？".to_string()))?;

    let mut c = Corpus::new();

    read_db_texts(&data, &mut c);
    read_event_texts(&data, &mut c);
    read_unique(&data, &mut c);
    read_plugins(game_dir, &mut c);

    Ok(c)
}

/// MZ 数据在 `<根>/data`，MV 在 `<根>/www/data`，两个都试。
fn data_dir(game_dir: &Path) -> Option<PathBuf> {
    [game_dir.join("data"), game_dir.join("www").join("data")]
        .into_iter()
        .find(|d| d.is_dir())
}

/// 读一个 `data/*.json` 成 `Value`。单个文件读坏不该拖垮整批，返回 `None`。
fn load_data(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let bytes = bytes
        .as_slice()
        .strip_prefix(b"\xEF\xBB\xBF".as_slice())
        .unwrap_or(&bytes);
    let text = String::from_utf8(bytes.to_vec()).ok()?;
    serde_json::from_str(&text).ok()
}

/// 一张表的记录数组。
///
/// 两种形状都认：
/// - `{"items": [ … ]}` —— MZ/MV 默认；
/// - **裸数组** `[null, { … }, …]` —— 有些游戏直接把 `$dataItems` 序列化成数组
///   （`SUCCUBUS DESIRE` 整份 `data/*.json` 都是这个形状）。只认前一种的话，
///   这类游戏的**每一张数据库表都会被静默跳过**，一条都收不到。
fn rows<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    if let Value::Array(a) = v {
        return a.as_slice();
    }
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// 数据库表 + `$dataSystem` 的名字/说明（和桥 `collectDbTexts` 一个口径）。
fn read_db_texts(data: &Path, c: &mut Corpus) {
    for t in DB_TABLES {
        let Some(v) = load_data(&data.join(t.file)) else { continue };
        for rec in rows(&v, t.key) {
            let Some(obj) = rec.as_object() else { continue };
            for f in t.fields {
                if let Some(s) = obj.get(*f).and_then(Value::as_str) {
                    take_db_string(c, s);
                }
            }
            // note 字段：MZ 里常有**会上屏的短文案**（物品/技能/状态/敌人注释）。
            // 纯 ASCII 的 `<插件标签>`/注释会被 `is_worthy` 过滤掉，不会大量进语料。
            if let Some(s) = obj.get("note").and_then(Value::as_str) {
                take_db_string(c, s);
            }
        }
    }

    // System.json：terms 四张子表 + 开关/变量等名字 + 标题。
    let Some(v) = load_data(&data.join("System.json")) else {
        return;
    };
    let Some(sys) = v.as_object() else { return };

    if let Some(terms) = sys.get("terms").and_then(Value::as_object) {
        for group in [
            "basic",
            "commands",
            "params",
            "messages",
            "skillTypes",
            "equipTypes",
            "armorTypes",
            "weaponTypes",
            "elements",
        ] {
            take_value(terms.get(group), c);
        }
    }
    for arr in SYS_ARRAYS {
        if let Some(a) = sys.get(*arr).and_then(Value::as_array) {
            for s in a.iter().filter_map(Value::as_str) {
                take_db_string(c, s);
            }
        }
    }
    // 属性（elements）名也常是会上屏的字：火/冰/雷/聖…
    if let Some(a) = sys.get("elements").and_then(Value::as_array) {
        for s in a.iter().filter_map(Value::as_str) {
            take_db_string(c, s);
        }
    }
    if let Some(s) = sys.get("gameTitle").and_then(Value::as_str) {
        take_db_string(c, s);
    }
}

/// 一条数据库文本：含 `\n` 的按整段收（说明是整串的），否则按单行收。
fn take_db_string(c: &mut Corpus, s: &str) {
    if s.contains('\n') {
        c.push(Granularity::Whole, s);
    } else {
        c.push(Granularity::Line, s);
    }
}

/// terms 里 basic/commands 是数组、messages 是对象，统一取字符串。
fn take_value(v: Option<&Value>, c: &mut Corpus) {
    match v {
        Some(Value::Array(a)) => {
            for s in a.iter().filter_map(Value::as_str) {
                take_db_string(c, s);
            }
        }
        Some(Value::Object(o)) => {
            for s in o.values().filter_map(Value::as_str) {
                take_db_string(c, s);
            }
        }
        _ => {}
    }
}

/// 事件文本（Map* / CommonEvents / Troops）：401/405 正文、101 说话人、102 选项、
/// 地图 displayName。和桥 `collectEventTexts` 一致：既有整段（连续 401 用 `\n`
/// 拼起来），也有单行。
fn read_event_texts(data: &Path, c: &mut Corpus) {
    let glob = glob_maps(data);
    for path in &glob {
        let Some(v) = load_data(path) else { continue };

        if let Some(name) = v.get("displayName").and_then(Value::as_str) {
            c.push(Granularity::Line, name);
        }
        // 地图 note：有时藏着会上屏的区域说明一类短文案。
        if let Some(s) = v.get("note").and_then(Value::as_str) {
            take_db_string(c, s);
        }
        if let Some(events) = v.get("events").and_then(Value::as_array) {
            for ev in events {
                // 事件名：名字标记插件/显示事件名的场景会用到（纯 ASCII 的 EV001 被过滤）。
                if let Some(name) = ev.get("name").and_then(Value::as_str) {
                    c.push(Granularity::Line, name);
                }
                let Some(pages) = ev.get("pages").and_then(Value::as_array) else { continue };
                for page in pages {
                    take_list(page.get("list").and_then(Value::as_array), c);
                }
            }
        }
    }

    // CommonEvents.json：commonEvents[].list
    if let Some(v) = load_data(&data.join("CommonEvents.json")) {
        for rec in rows(&v, "commonEvents") {
            take_list(rec.get("list").and_then(Value::as_array), c);
        }
    }
    // Troops.json：troops[].pages[].list（战斗事件只有开打才载入，离线扫得到）
    if let Some(v) = load_data(&data.join("Troops.json")) {
        for rec in rows(&v, "troops") {
            if let Some(pages) = rec.get("pages").and_then(Value::as_array) {
                for page in pages {
                    take_list(page.get("list").and_then(Value::as_array), c);
                }
            }
        }
    }
}

/// `data/Map\d+.json` 的路径列表。
fn glob_maps(data: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name.len() > 4 && name.starts_with("Map") && name.ends_with(".json") {
            if name[3..name.len() - 5].chars().all(|ch| ch.is_ascii_digit()) {
                out.push(path);
            }
        }
    }
    out
}

/// 一条事件指令里玩家能看到的文本。10x 这几个码在指令表里是平铺的，不递归。
fn take_command(cmd: &Value, c: &mut Corpus) {
    let Some(p) = cmd.get("parameters").and_then(Value::as_array) else { return };
    let code = cmd.get("code").and_then(Value::as_u64).unwrap_or(0);
    match code {
        // 401 对话正文 / 405 滚动正文
        401 | 405 => {
            if let Some(s) = p.first().and_then(Value::as_str) {
                push_lines(c, s);
            }
        }
        // 101 说话人名（第 5 个参数）
        101 => {
            if let Some(s) = p.get(4).and_then(Value::as_str) {
                push_lines(c, s);
            }
        }
        // 102 选项
        102 => {
            if let Some(a) = p.first().and_then(Value::as_array) {
                for s in a.iter().filter_map(Value::as_str) {
                    push_lines(c, s);
                }
            }
        }
        _ => {}
    }
}

/// 一段指令列表：收每条单行，也把连续 401 拼成整段收一份。
/// 整段对得上 `join('\n')` 那个查表 key，只收单行的话整段永远查不中。
fn take_list(list: Option<&Vec<Value>>, c: &mut Corpus) {
    let Some(list) = list else { return };
    let mut buf: Vec<String> = Vec::new();
    for cmd in list {
        take_command(cmd, c);
        let is_401 = cmd.get("code").and_then(Value::as_u64) == Some(401)
            && cmd.get("parameters").and_then(Value::as_array)
                .and_then(|p| p.first().and_then(Value::as_str))
                .is_some();
        if is_401 {
            let s = cmd.get("parameters").and_then(Value::as_array)
                .and_then(|p| p.first().and_then(Value::as_str))
                .unwrap();
            buf.extend(s.split('\n').map(str::to_string));
        } else if !buf.is_empty() {
            c.push(Granularity::Whole, &buf.join("\n"));
            buf.clear();
        }
    }
    if !buf.is_empty() {
        c.push(Granularity::Whole, &buf.join("\n"));
    }
}

/// 按 `\n` 拆开逐行收（消息窗口是逐行查表的，key 也得是行）。
fn push_lines(c: &mut Corpus, s: &str) {
    for part in s.split('\n') {
        c.push(Granularity::Line, part);
    }
}

/// 独特数据：`data/unique/*.json`。整棵走一遍，把该跳的键挑出去。
/// 和桥 `collectDuTexts` 一个口径，只是 HeroineTalk 那类按行收。
fn read_unique(data: &Path, c: &mut Corpus) {
    let dir = data.join("unique");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(v) = load_data(&path) else { continue };
        walk_unique(&v, c);
    }
}

/// 递归收字符串。字符串按单行收（拆 `\n`）。
fn walk_unique(v: &Value, c: &mut Corpus) {
    match v {
        Value::String(s) => push_lines(c, s),
        Value::Array(a) => {
            for x in a {
                walk_unique(x, c);
            }
        }
        Value::Object(o) => {
            for (k, val) in o {
                if SD_SKIP.contains(&k.as_str()) {
                    continue;
                }
                walk_unique(val, c);
            }
        }
        _ => {}
    }
}

/// 插件文本：`js/plugins.js` 的参数 + `js/plugins/*.js` 的源码字面量。
///
/// 两处都要收，缺一个就会整批漏文本：
///
/// - `plugins.js` 是插件**清单 + 参数**，SceneCustomMenu 那类自定义菜单把菜单项
///   文字写在参数里（`Scene1 → "\I[188]手帳"`），桥要等画面真画到才收到。
/// - `js/plugins/*.js` 是插件**源码**，游戏常把大段文案硬编码在这儿。本游戏
///   （サキュバスアカデミア）的 177 个 `C_*.js` 就是每个角色的数据，描述成
///   `shortDescription: ['傲慢な猫が変化したマドロミ。', …]` 这样的字符串数组 ——
///   `plugins.js` 里对应的 `parameters` 是空的 `{}`，光看清单一条也收不到。
///
/// MV 在 `www/js/`，MZ 在 `js/`，两个根都试，认第一个有 `plugins.js` 的。
fn read_plugins(game_dir: &Path, c: &mut Corpus) {
    let Some(root) = [game_dir.to_path_buf(), game_dir.join("www")]
        .into_iter()
        .find(|r| r.join("js").join("plugins.js").is_file())
    else {
        return;
    };

    // 1) 清单参数。抽取规则集中在 `crate::corpus::plugins`——尤其「参数值本身是可执行
    //    JS」时会把 JS 外壳挡掉、只收内层字符串字面量。
    if let Ok(src) = std::fs::read_to_string(root.join("js").join("plugins.js")) {
        let ex = Extractor::default(); // 黑白名单暂空：含日文即收，JS 外壳挡外。
        let extracted = extract_config(&src, &ex).unwrap_or_default();
        for s in &extracted.candidates {
            c.push(Granularity::Fragment, s);
        }
    }

    // 2) 源码字面量。
    read_plugin_sources(&root.join("js").join("plugins"), c);

    // 3) 补丁/模组脚本。MV 游戏常把新增文案写在 `patch/`、`mod/` 下 —— 它们不在
    //    `js/` 里、也不在 `plugins.js` 清单里，靠插件运行时按文件名加载，
    //    前面两步一条也碰不到。目录不存在就自动跳过。
    for extra in ["patch", "mod"] {
        read_plugin_sources(&root.join(extra), c);
    }
}

/// 扫一个目录下的 `*.js`，抽出含日文的字符串字面量。
///
/// 按**单行**收：这些文案是数组里的一个个元素，游戏拼起来画成多行块，而
/// `Dict::translate` 整段查不到会退到逐行 exact，按行收就够。
fn read_plugin_sources(dir: &Path, c: &mut Corpus) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("js") {
            continue;
        }
        // 桥自己的源码不扫：里头是 wtool 的日志文案，收了只会污染语料。
        if path.file_name().and_then(|n| n.to_str()) == Some("wtool_bridge.js") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else { continue };
        for s in extract_string_literals(&src) {
            c.push(Granularity::Line, &s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wtool_offline_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 一张带事件的最小 Map 文件 + System.json。
    fn make_game(dir: &PathBuf) {
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(
            dir.join("data").join("Map001.json"),
            r#"{"displayName":"村の入り口","events":[{"pages":[{"list":[
                {"code":101,"parameters":[0,0,0,0,"村長"]},
                {"code":401,"parameters":["これはテスト\n二行目です"]},
                {"code":102,"parameters":[["はい","いいえ"],0,0,0,0]}
            ]}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("data").join("System.json"),
            r#"{"gameTitle":"サンプル","terms":{"basic":["HP","MP","状態"],"messages":{"alwaysDash":"常時ダッシュ"}},"switches":["スイッチ1","スイッチ2"]}"#,
        )
        .unwrap();
    }

    /// 事件文本/displayName/DB 词条/菜单词条都能扫出来。
    #[test]
    fn scan_collects_event_and_db_and_terms() {
        let dir = scratch("ev");
        make_game(&dir);
        let c = scan(&dir).unwrap();

        // 事件正文：单行（4 行）+ 整段（2 行 401 拼起来）
        assert!(c.flat().iter().any(|(g, s)| *g == Granularity::Whole && s == "これはテスト\n二行目です"));
        assert!(c.flat().iter().any(|(g, s)| *g == Granularity::Line && s == "これはテスト"));
        // 说话人 + 选项
        assert!(c.flat().iter().any(|(g, s)| *g == Granularity::Line && s == "村長"));
        assert!(c.flat().iter().any(|(g, s)| *g == Granularity::Line && s == "はい"));
        // displayName
        assert!(c.flat().iter().any(|(g, s)| *g == Granularity::Line && s == "村の入り口"));
        // 菜单词条（纯 ASCII 的 HP 被过滤掉了）
        assert!(c.flat().iter().any(|(g, s)| *g == Granularity::Line && s == "状態"));
        assert!(!c.flat().iter().any(|(_, s)| s == "MP"));
        // 开关名
        assert!(c.flat().iter().any(|(_, s)| s == "スイッチ1"));
    }

    /// 插件参数里的自定义菜单文字（SceneCustomMenu 那种）要扫成片段。
    #[test]
    fn scan_collects_plugin_parameters_as_fragment() {
        let dir = scratch("plugin");
        make_game(&dir);
        std::fs::create_dir_all(dir.join("js")).unwrap();
        std::fs::write(
            dir.join("js").join("plugins.js"),
            r#"var $plugins = [{"name":"SceneCustomMenu","status":true,"parameters":{"Scene1":"\\I[188]手帳","Scene2":"強化メニュー"}}];"#,
        )
        .unwrap();

        let c = scan(&dir).unwrap();
        assert!(c.flat().iter().any(|(g, s)| *g == Granularity::Fragment && s == "\\I[188]手帳"));
        assert!(c.flat().iter().any(|(g, s)| *g == Granularity::Fragment && s == "強化メニュー"));
    }

    /// 去重：同一段台词既整段收又单行收，计数区分，unique 才是真数量。
    #[test]
    fn scan_dedup_counting_works() {
        let dir = scratch("dedup");
        make_game(&dir);
        let c = scan(&dir).unwrap();
        // 至少收进来若干条；total 是 u64，unique 是 usize，转成同型再比。
        assert!(c.unique() > 0);
        assert!(c.total() >= c.unique() as u64);
    }
}