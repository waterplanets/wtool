//! 插件参数文本抽取（独立、可替换）。
//!
//! 普通游戏文本存在 `data/*.json` 里，是一行一行给人看的字。但 RPG Maker 有些
//! 插件会把「文本」以**字符串形式**塞进 JS 源码，例如：
//!
//! ```js
//! parameters: { "Text": "(() => { if (level>=50) return \"この世界には…\"; })()" }
//! ```
//!
//! 这种值的字符串**内容是一段可执行 JS**，不能整段当词条（整段喂模型/回写替换都会
//! 让插件 `eval` 崩）。真正的文案藏在 JS 的**字符串字面量**里（`return "…"`）。
//!
//! 本模块负责从「插件里的文本」抠出候选，是 `offline.rs` 的插件采集解耦出去的一块。
//! 两个来源：`js/plugins.js` 的**参数**（[`extract_config`]），和 `js/plugins/*.js`
//! 的**源码字面量**（[`extract_string_literals`]，游戏常把角色/敌人文案硬编码在
//! 这些文件里）。**不同游戏的插件写法可能不同**，这里给一个保守的默认实现 +
//! 可配置的黑/白名单，遇到特殊游戏单独调这里的规则即可，不影响其它采集。
//!
//! 两部分解析用同一种「跳过引号、数括号配平」的手写扫描（这是万能法，不依赖某个
//! 具体引擎，所以对 MV / MZ、对新旧引擎版本都稳）。
//!
//! - [`parse_parameters`]：把 `plugins.js` 里 `"parameters"` 对象的裸 JSON 串抠成
//!   [`Value`]（一个配平的 `{…}`）。
//! - [`extract_config`]：把整份 `plugins.js` 的候选文本抠出来，JS 外壳挡在外面，
//!   只放内层字面量进结果。

use serde_json::Value;

use crate::error::Result;

/// 从插件参数值里抽取候选文本的规则。
///
/// 黑白名单现在默认全空（配空就退化成「每个含日文的字面量都收」）。将来某个游戏
/// 的某个 `Id`/字段不该翻，往里塞一条即可 —— 不用改抽取逻辑，只改配置。
#[derive(Debug, Default, Clone)]
pub struct Extractor {
    /// key 或 `Id` 命中这些值的参数项，整项不翻。
    pub blacklist: Vec<String>,
    /// 非空时：只翻 `Id`/key 命中这些值的参数项。
    pub whitelist: Vec<String>,
}

/// 一份 `plugins.js` 的抽取结果。
#[derive(Default)]
pub struct Extracted {
    /// 候选文本（JS 外壳已剔除，只含可以翻的字面量）。
    pub candidates: Vec<String>,
    /// 因为被判定成「JS 外壳」而挡下来、没有整段入语的条目数（仅统计用）。
    pub shell_skipped: usize,
}

/// 判定某参数值字符串「像可执行 JS」而不是「给人看的整句」。
///
/// 这是启发式：见到 return/箭头/圆括号自执行/函数关键字，就当真 JS 处理——
/// 进去抽字面量；否则当普通文本整段收。
fn looks_like_js(s: &str) -> bool {
    s.contains("=>")
        || s.contains("function")
        || s.contains("return ")
        || s.contains("((")
}

/// 从一段 JS 源码里抽出字符串字面量的**内容**（去引号、解转义）。
///
/// 返回值里我们只留含日文的（那些才是要翻的字），余下的（`"\\\\c[14]"`、空串、
/// 纯 ASCII 等）丢弃。这是「动态 JS 块内有效文本的提取」，和 `translate` 里
/// `mask` 的思路相反但成型独立：这里要剥的是引号，那边要保的只有控制码。
///
/// 两种引号 + 反引号都认：`plugins.js` 里的参数值都是双引号，插件**源文件**
/// （`js/plugins/C_*.js` 那批）清一色单引号，而 `patch/`、`mod/` 里的补丁脚本
/// 还常用模板串（`` `チュートリアル中です\n…` ``）。
pub(crate) fn extract_string_literals(js: &str) -> Vec<String> {
    let bytes = js.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        // 跳过注释。注释里的撇号（`// don't` 这种）会开一个假字符串，一路吞到
        // 下一个引号，把大半段代码当文案收进来 —— 插件源码里垃圾条目的主因。
        if bytes[i] == b'/' && i + 1 < n {
            if bytes[i + 1] == b'/' {
                i += 2;
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                i += 2;
                while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                continue;
            }
        }
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' && quote != b'`' {
            i += 1;
            continue;
        }
        // 读到引号开头，往后解一个字符串字面量。
        i += 1;
        let mut buf: Vec<u8> = Vec::new();
        let mut closed = false;
        let mut raw_nl = false;
        while i < n {
            let c = bytes[i];
            if c == b'\\' {
                // 转义：把 \n \t \\ \" 之类解成对应字符。
                if i + 1 < n {
                    let e = bytes[i + 1];
                    let replaced = match e {
                        b'n' => Some(b'\n'),
                        b't' => Some(b'\t'),
                        b'r' => Some(b'\r'),
                        b'0' => Some(b'\0'),
                        b'\\' => Some(b'\\'),
                        b'"' => Some(b'"'),
                        b'\'' => Some(b'\''),
                        _ => None,
                    };
                    match replaced {
                        Some(b) => buf.push(b),
                        None => {
                            buf.push(b'\\');
                            buf.push(e);
                        }
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            } else if c == quote {
                i += 1;
                closed = true;
                break; // 字符串结束
            } else {
                // 单/双引号串里出现**裸换行**是 JS 语法错误，只可能是引号配错了。
                if c == b'\n' || c == b'\r' {
                    raw_nl = true;
                }
                buf.push(c);
                i += 1;
            }
        }
        // 引号没闭合 = 这根本不是字符串，是别处的撇号（正则、被截断的代码）。
        // 这种「串」一吞就是大半段代码，丢掉比收进来安全。
        if !closed {
            continue;
        }
        // 同上：`'…'`/`"…"` 里不可能有裸换行，有就是配错了（模板串可以有）。
        if raw_nl && quote != b'`' {
            continue;
        }
        // 到这儿才把字节转成字符串：**中途不能 `byte as char`**，那会把一个 UTF-8
        // 多字节序列拆成一串 Latin-1 字符（`相手` 变成 `ç›¸æ‰‹`），`has_japanese`
        // 于是全判假，JS 函数体里的字面量一条都收不到。保留字节、出引号再解码。
        let s = String::from_utf8_lossy(&buf);
        // 只收含日文、且非空有实际内容的。
        if has_japanese(&s) {
            out.push(s.into_owned());
        }
    }
    out
}

/// 字符串里是否含日文字（含中文/日文假名区）。
fn has_japanese(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c,
            '\u{3040}'..='\u{309f}' | // 平假名
            '\u{30a0}'..='\u{30ff}' | // 片假名
            '\u{3000}'..='\u{303f}' | // 日文标点
            '\u{3400}'..='\u{4dbf}' | // CJK 扩展 A
            '\u{4e00}'..='\u{9fff}'   // 常用汉字
        )
    })
}

/// 平铺一个参数 `Value`，产出候选文本。
///
/// 对每个字符串值：
/// - 能解析成 JSON 的（`[`/`{` 壳的配置块）→ 递归进去取值，不整段收；
/// - 像 JS 函数的 → 进 `extract_string_literals` 抽内层字面量；
/// - 带代码痕迹、养活注释/赋值的（`//`、`;`、`=>`…）→ 也当机器处理只抽字面量；
/// - 否则（普通给人看的字）→ 整段入候选。
fn walk(v: &Value, ex: &Extractor, out: &mut Vec<String>, shell: &mut usize) {
    match v {
        Value::String(s) => {
            if blacklisted(s, ex) {
                return;
            }
            // 配置块：它自己是个 JSON 串（`[...]`/`{...}`），递归进去，别整坨收。
            if s.trim_start().starts_with(['[', '{'])
                && serde_json::from_str::<Value>(s).is_ok()
            {
                if let Ok(inner) = serde_json::from_str::<Value>(s) {
                    walk(&inner, ex, out, shell);
                }
                return;
            }
            if looks_like_js(s) || looks_like_code(s) {
                *shell += 1;
                for lit in extract_string_literals(s) {
                    out.push(lit);
                }
            } else {
                out.push(s.clone());
            }
        }
        Value::Array(a) => {
            for x in a {
                walk(x, ex, out, shell);
            }
        }
        Value::Object(o) => {
            if let Some(id) = o.get("Id").and_then(Value::as_str) {
                if !whitelisted(id, ex) {
                    return; // 白名单非空且没命中 → 这项整体不翻
                }
            }
            for (k, val) in o {
                if blacklisted_key(k, ex) {
                    continue;
                }
                walk(val, ex, out, shell);
            }
        }
        _ => {}
    }
}

/// 字符串是不是代码味（JS 之外的机器文本）：注释、赋值、语句。
fn looks_like_code(s: &str) -> bool {
    s.contains("//") || s.contains("/*") || s.contains(';') || s.contains("var ")
        || s.contains("let ") || s.contains("const ")
}

/// 一个字符串值本身命中黑名单条目 → 跳过。
fn blacklisted(s: &str, ex: &Extractor) -> bool {
    ex.blacklist.iter().any(|b| s == b)
}

/// 一个对象的 key 命中黑名单条目 → 跳过这个 key 的整值。
fn blacklisted_key(k: &str, ex: &Extractor) -> bool {
    ex.blacklist.iter().any(|b| k == b)
}

/// `Id` 是否放行：白名单空则全放行，否则只有命中才放行。
fn whitelisted(id: &str, ex: &Extractor) -> bool {
    ex.whitelist.is_empty() || ex.whitelist.iter().any(|w| id == w)
}

/// 从整份 `plugins.js` 文本抽出候选文本。
///
/// `Extractor::default()` 黑白名单全空 = 尽量全收（含日文即收），正好对应现在的
/// 行为，只是多了「JS 外壳挡在外」这一层。将来某游戏要挑，传一个配好的
/// `Extractor` 进来即可，本函数签名不变。
pub fn extract_config(src: &str, ex: &Extractor) -> Result<Extracted> {
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut shell = 0usize;
    let mut i = 0;

    while i < n {
        let Some(rel) = src[i..].find("\"parameters\"") else { break };
        let at = i + rel;
        let Some(open) = src[at..].find('{') else { break };
        let start = at + open;
        let Some(close) = balanced_close(src, start) else {
            i = start + 1;
            continue;
        };
        if let Ok(v) = serde_json::from_str::<Value>(&src[start..=close]) {
            walk(&v, ex, &mut out, &mut shell);
        }
        i = close + 1;
    }

    Ok(Extracted {
        candidates: out,
        shell_skipped: shell,
    })
}

/// 从 `start`（`{` 处）往后找配平的 `}`，跳过引号内的内容。找不到返回 `None`。
///
/// 这是纯扫描、不解析 JS 语法，所以对任何引擎生成的 `plugins.js` 格式都适用。
fn balanced_close(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut depth = 0i32;
    let mut i = start;
    while i < n {
        if bytes[i] == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex() -> Extractor {
        Extractor::default()
    }

    #[test]
    fn dynamic_js_text_extracts_inner_literals() {
        let src = r#"var $plugins=[{"name":"X","status":true,"parameters":{"Text":"(() => {\n\treturn \"この世界にはもう、俺を殺せるモノは存在しないだろう\";\n\treturn \"\"\n})()"}}];"#;
        let e = extract_config(src, &ex()).unwrap();
        // 外壳整段不入，只有 return 里的日文句进来。
        assert!(!e.candidates.iter().any(|s| s.contains("(() =>")));
        assert!(e.candidates.contains(&"この世界にはもう、俺を殺せるモノは存在しないだろう".to_string()));
        assert!(e.shell_skipped >= 1);
    }

    /// 插件**源文件**里的字面量：单引号（`C_*.js` 的文案数组）、反引号
    /// （`patch/*.js` 的模板串）都要能抽出来。
    #[test]
    fn source_literals_handle_single_quote_and_backtick() {
        let src = "var X = {\n  shortDescription: [\n    '傲慢な猫が変化したマドロミ。',\n    '飼われていたころから人間を下僕と見ていたが、',\n  ],\n};\nvar y = `チュートリアル中です`;\nvar z = 'OK';\nvar broken = it's fine;\n";
        let got = extract_string_literals(src);
        assert!(got.contains(&"傲慢な猫が変化したマドロミ。".to_string()));
        assert!(got.contains(&"飼われていたころから人間を下僕と見ていたが、".to_string()));
        assert!(got.contains(&"チュートリアル中です".to_string()));
        // 纯 ASCII 的照旧丢弃。
        assert!(!got.contains(&"OK".to_string()));
        // 没闭合的撇号不能把后面一大段代码吞成一个「字符串」。
        assert!(!got.iter().any(|s| s.contains("var ")));
    }

    #[test]
    fn plain_text_stays_whole() {
        let src = r#"var $plugins=[{"name":"SceneCustomMenu","status":true,"parameters":{"Scene1":"\\I[188]手帳","Scene2":"強化メニュー"}}];"#;
        let e = extract_config(src, &ex()).unwrap();
        assert!(e.candidates.contains(&"\\I[188]手帳".to_string()));
        assert!(e.candidates.contains(&"強化メニュー".to_string()));
        assert_eq!(e.shell_skipped, 0);
    }

    #[test]
    fn blacklist_drops_matching() {
        let src = r#"var $plugins=[{"name":"X","status":true,"parameters":{"Token":"\\V[1]","Keep":"ここは残す"}}];"#;
        let mut cfg = Extractor::default();
        cfg.blacklist.push("Token".to_string());
        let e = extract_config(src, &cfg).unwrap();
        assert!(e.candidates.contains(&"ここは残す".to_string()));
        assert!(!e.candidates.contains(&"\\V[1]".to_string()));
    }

    #[test]
    fn whitelist_restricts_by_id() {
        let src = r#"var $plugins=[{"name":"X","status":true,"parameters":[{"Id":"A","Text":"甲"},{"Id":"B","Text":"乙"}]}];"#;
        let mut cfg = Extractor::default();
        cfg.whitelist.push("B".to_string());
        let e = extract_config(src, &cfg).unwrap();
        assert!(!e.candidates.contains(&"甲".to_string()));
        assert!(e.candidates.contains(&"乙".to_string()));
    }

    /// JSON 配置壳（`[`/`{` 开头）不能整坨当词条，递归进去，代码味叶子抽出字面量。
    #[test]
    fn json_config_shell_is_not_kept_whole() {
        let src = r#"var $plugins=[{"name":"X","status":true,"parameters":{"Gauges":"[{ \"Id\":\"map_gauge_VP\", \"Script\":\"battler.hp; // 最大HP\" },{\"Label\":\"ちから\"}]"}}];"#;
        let e = extract_config(src, &ex()).unwrap();
        // 整坨 JSON 配置串不能进候选（没有这种巨大的 key）。
        assert!(!e.candidates.iter().any(|s| s.contains("map_gauge_VP")));
        // 干净的日文叶子照收（"ちから"）。
        assert!(e.candidates.contains(&"ちから".to_string()));
        // 代码注释里的日本注解 "// 最大HP"：整段被剥离成字面量，仅 "最大HP" 可能留下，
        // 但不应出现含 `//` 的原始行。
        assert!(!e.candidates.iter().any(|s| s.contains("//")));
    }
}