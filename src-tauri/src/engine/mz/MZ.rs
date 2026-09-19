//! RPG Maker MZ 适配器。
//!
//! MZ 沿用了 MV 的消息系统类名——`Window_Message.prototype.startMessage`、
//! `$gameMessage._texts`——所以这份实现对 MV 同样成立。两者只差插件目录：
//! MZ 是 `js/plugins/`，MV 是 `www/js/plugins/`。
//!
//! 这一层只干一件事：**词表翻译**。把 [`Engine`] 的语义翻成协议上的 JSON，
//! 字节怎么走交给 [`Link`]，游戏里真正的动作交给 `wtool_bridge.js`。
//! 于是：换引擎不动传输，换传输（TCP → CDP → DLL）不动引擎。
//!
//! 线上的协议（两边改一处就得同步改另一处）：
//!
//! ```text
//!   游戏 → wtool
//!     {"ev":"hello","engine":"mz"}                    连上就发
//!     {"ev":"text","seq":1,"lines":["…"],"text":"…"}  一段对话出现
//!     {"ev":"closed","seq":1}                         那段对话结束
//!     {"ev":"terms","texts":["…"]}                    问一批菜单词条（开机一次）
//!     {"ev":"terms","loose":true,"label":"…","texts":["…"]}
//!                                                     同上，但要退级查（数据库文本、
//!                                                     事件文本、独特数据）
//!
//!   wtool → 游戏
//!     {"cmd":"current"}                               问一次「现在显示的是什么」
//!     {"cmd":"apply","seq":1,"from":"原文","text":"译文"}
//!     {"cmd":"terms","map":{"原文":"译文"}}            回问过来那批词条里翻得出的
//! ```

use serde_json::{json, Map, Value};

use crate::engine::link::Link;
use crate::engine::{Engine, Event};
use crate::error::{Error, Result};
use crate::model::Dialogue;

/// wtool → 游戏 的命令名。
const CMD_CURRENT: &str = "current";
const CMD_APPLY: &str = "apply";
const CMD_TERMS: &str = "terms";

/// RPG Maker MZ（同时也是 MV）。
///
/// 泛型参数是传输，不是引擎——`MzEngine<TcpLink>`、将来的 `MzEngine<CdpLink>`
/// 是同一个引擎的两个接线方式。
pub struct MzEngine<L: Link> {
    link: L,
    /// 最近一条 `Text` 事件。`apply` 靠它知道该写回哪一段、原文是什么。
    last: Option<Dialogue>,
}

impl<L: Link> MzEngine<L> {
    pub fn new(link: L) -> Self {
        Self { link, last: None }
    }

    /// 主动问一次「现在显示的是什么」。
    ///
    /// 平时用不上：游戏侧是推送式的。这个是断线重连、或者业务层刚起来
    /// 却发现游戏已经显示着对话框时，用来补一次的。
    pub fn refresh(&mut self) -> Result<()> {
        self.link.send(&json!({ "cmd": CMD_CURRENT }).to_string())
    }

    /// 解析游戏推来的一行。
    ///
    /// 认不出来的返回 `Ok(None)` 而不是报错：游戏侧将来加了新事件，
    /// 老版本 wtool 应该当没看见，而不是被打挂。
    fn parse(line: &str) -> Result<Option<Event>> {
        let v: Value = serde_json::from_str(line)?;

        Ok(match v.get("ev").and_then(Value::as_str).unwrap_or("") {
            // 游戏侧不知道 wtool 的版本，wtool 先从它这儿知道自己面对的是什么。
            "hello" => Some(Event::Attached),

            "text" => {
                let lines = strings(&v, "lines");
                // text 缺失或和 lines 对不上时，一律以 lines 为准——
                // 这个串就是查词典的 key，必须和游戏侧算出来的完全一致。
                let text = lines.join("\n");
                Some(Event::Text(Dialogue {
                    seq: v.get("seq").and_then(Value::as_u64).unwrap_or(0),
                    lines,
                    text,
                }))
            }

            "closed" => Some(Event::Closed {
                seq: v.get("seq").and_then(Value::as_u64).unwrap_or(0),
            }),

            "terms" => Some(Event::Terms {
                // 缺省是精确查：游戏侧只在物品/技能那批上显式带 loose。
                loose: v.get("loose").and_then(Value::as_bool).unwrap_or(false),
                // 只用来打日志，认不出来也不影响翻译，所以随便给个兜底。
                label: v
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("整批文本")
                    .to_string(),
                texts: strings(&v, "texts"),
            }),

            _ => None,
        })
    }
}

/// 取一个字段里的字符串数组，非字符串的项直接丢掉。
fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl<L: Link> Engine for MzEngine<L> {
    fn name(&self) -> &'static str {
        "RPG Maker MZ"
    }

    fn poll(&mut self) -> Result<Option<Event>> {
        loop {
            let Some(line) = self.link.poll()? else {
                return Ok(None);
            };
            // 认不出来就继续往下取，别让一条无关的消息把循环卡住一拍。
            let Some(ev) = Self::parse(&line)? else {
                continue;
            };

            match &ev {
                Event::Text(d) => self.last = Some(d.clone()),
                Event::Closed { .. } => self.last = None,
                Event::Attached | Event::Terms { .. } => {}
            }
            return Ok(Some(ev));
        }
    }

    fn apply(&mut self, target: &str) -> Result<()> {
        let d = self
            .last
            .as_ref()
            .ok_or_else(|| Error::new("当前没有对话，没有可写的地方"))?;

        // from 也发过去：游戏侧靠它把「原文 → 译文」记进本地缓存，
        // 下次同样文本出现时就能在画之前同步替换，不用等这一趟往返。
        let msg = json!({
            "cmd": CMD_APPLY,
            "seq": d.seq,
            "from": d.text,
            "text": target,
        });
        self.link.send(&msg.to_string())
    }

    fn apply_terms(&mut self, map: &[(String, String)]) -> Result<()> {
        // 空 map 也照发：游戏侧收到会替换 0 条，等于什么也没做，
        // 但日志上「问过、没翻上」这件事就留下来了。
        let obj: Map<String, Value> = map
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();

        let msg = json!({ "cmd": CMD_TERMS, "map": Value::Object(obj) });
        self.link.send(&msg.to_string())
    }
}
