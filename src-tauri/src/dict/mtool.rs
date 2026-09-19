//! MTool 词典（`TrsData.bin`）适配器 —— 防腐层。
//!
//! 一个实测出来的事实决定了这里的全部设计：**`TrsData.bin` 不是「整段词典」，
//! 是个碎片袋**。同一句话的整段、单行、甚至句子里的一个片段，各自都可能是
//! 独立的 key —— 比如 `と戦闘しますか？` 和 `"？？？"と戦闘しますか？` 同时存在。
//!
//! 所以查表要按粒度退级：**整段 → 单行 → 片段**。前两级是精确匹配，
//! 第三级是「最长优先、从左往右」的片段替换。

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::Result;
use crate::model::TsSentence;

/// 建好索引的词典。
pub struct Dict {
    map: BTreeMap<String, String>,
    /// 词典里出现过的 key 长度（字符数），**降序去重**。
    ///
    /// 片段匹配每个位置都要试一串长度，与其从「最长 key」一路往下试
    /// （实测最长 126 字符，绝大多数长度根本不存在），不如直接只试
    /// 真实存在的档位，并且天然就是「最长优先」。
    lens: Vec<usize>,
}

impl Dict {
    /// 读词典并建好索引：原文 → 译文。
    ///
    /// 翻译循环每句话都要查一次，1.6 MB 的词典扛不住线性扫，所以查表用的是
    /// `BTreeMap`（几次字符串比较），而不是把整份转成 `Vec` 再从头找。
    pub fn load(path: &Path) -> Result<Self> {
        let map = load_index(path)?;

        let mut lens: Vec<usize> = map.keys().map(|k| k.chars().count()).collect();
        lens.sort_unstable_by(|a, b| b.cmp(a));
        lens.dedup();

        Ok(Self { map, lens })
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 精确查一条。
    ///
    /// 译文为空视为「词典没翻这条」，跟查不到一样返回 `None` —— 这样才会
    /// 继续往下一级退。实测词典里有 591 条空译文，都是没翻的占位。
    ///
    /// 菜单词条（`System.json` 的 `terms`）也走这个口子：词条本身就是词典的
    /// key 粒度（`アイテム`、`レベル` 这种短词），整条查不中就是没有，
    /// 退级到片段反而会把 `ーーーー` 这种分隔符翻出鬼来。
    /// 词典查不到时，再兜一层内置的战斗消息模板（见 `message_template`）。
    pub fn exact(&self, key: &str) -> Option<&str> {
        self.map
            .get(key)
            .map(String::as_str)
            .filter(|t| !t.is_empty())
            .or_else(|| message_template(key))
    }

    /// 查一段对话要显示的文本，返回该显示什么。
    ///
    /// 三级退级，一个字都没翻上时返回 `None`（调用方按「未命中」记账）：
    ///
    /// 1. **整段**：`text` 原样查一次，这是最干净的情况。
    /// 2. **单行**：按 `\n` 拆开，逐行精确查。
    /// 3. **片段**：单行也没中，就在这一行里做「最长优先」的片段替换。
    ///
    /// 三级是**逐行**叠加的，不是全有全无：某一行整行命中、另一行只翻出
    /// 半截，这很常见，能翻多少算多少。
    pub fn translate(&self, text: &str) -> Option<String> {
        // 第一级：整段。
        if let Some(t) = self.exact(text) {
            return Some(t.to_string());
        }

        // 第二级 + 第三级：逐行退级。
        let mut out = String::with_capacity(text.len() + 16);
        let mut any = false;
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match self.exact(line) {
                Some(t) => {
                    out.push_str(t);
                    any = true;
                }
                None => match self.fragments(line) {
                    Some(t) => {
                        out.push_str(&t);
                        any = true;
                    }
                    // 这一行翻不动，原样搬过去。
                    None => out.push_str(line),
                },
            }
        }

        if any {
            Some(out)
        } else {
            None
        }
    }

    /// 片段替换：最长优先、从左往右。
    ///
    /// 扫到哪个字符就在那儿试「以它为开头的、词典里真有的那些长度」，
    /// 从长的开始试，中一个就整段替换掉、指针跳过这么多个字符；
    /// 一个都不中就原样搬走一个字符，从下一个字符接着试。
    ///
    /// **纯 ASCII 的 key 不参与这一级**。片段匹配是在日本字里找日本片段，
    /// 而纯 ASCII 的 key 只可能在控制码里撞上：词典里有一条 `"10" -> "用10"`
    /// （MTool 自己存的脏数据），游戏里 `\V[1001]`（所持 Mag 变量）就会在 `10`
    /// 那儿中招，变成 `\V[用1001]`，引擎解析不出来就把原文样扔屏幕上 ——
    /// 玩家看到的是 `[用1001]` 这种乱码。这样的 key 全词典只有 10 条，8 条译文
    /// 为空（本来就不参与）、2 条是恒等映射（`"B" -> "B"`），一条真翻译都不丢。
    ///
    /// 整段和单行两级不受影响，照原样查 —— 一行真的就是 `10` 时，词典说什么
    /// 就是什么。
    ///
    /// 返回 `None` 表示这行一个能用的片段都没找到。
    fn fragments(&self, line: &str) -> Option<String> {
        // 每个字符的起始字节偏移。要按字符切、又要拿 `&str` 去查表，
        // 所以先算好边界；之后的循环里就只剩字节切片，不再逐次数字符。
        let at: Vec<usize> = line.char_indices().map(|(b, _)| b).collect();
        let n = at.len();

        let mut out = String::with_capacity(line.len() + 16);
        let mut any = false;
        let mut c = 0usize;
        while c < n {
            // lens 是降序的：第一个试中的就是最长的那个。
            let mut hit: Option<(usize, &str)> = None;
            for &k in &self.lens {
                let rest = n - c;
                if k > rest {
                    continue;
                }
                let end = if k < rest { at[c + k] } else { line.len() };
                let s = &line[at[c]..end];
                if s.is_ascii() {
                    continue;
                }
                if let Some(t) = self.exact(s) {
                    hit = Some((k, t));
                    break;
                }
            }

            match hit {
                Some((k, t)) => {
                    out.push_str(t);
                    any = true;
                    c += k;
                }
                None => {
                    let end = if c + 1 < n { at[c + 1] } else { line.len() };
                    out.push_str(&line[at[c]..end]);
                    c += 1;
                }
            }
        }

        if any {
            Some(out)
        } else {
            None
        }
    }
}

/// 战斗消息模板（`$dataSystem.terms.messages`）的内置对照表。
///
/// 这批串词典里**一条都没有**，不是漏翻，是 MTool 的存法决定的：它按运行时
/// **拼好**的整句存 key（`リリス の 50 の経験値を得た`），角色名/数值一换 key
/// 就变，带 `%1` 的模板自己从来没进过词典。而战斗日志
/// （`Window_BattleLog`）每一行都是 `TextManager.message(id)` 取模板再
/// `format` 填变量，所以**把模板换成中文，日志整条就是中文** —— 变量那部分
/// （角色名、物品名、参数名）本来就已经被翻过了，不用另做处理。
///
/// 退级（片段匹配）能凑出半截，但日语助词夹在变量中间（`%1は%2を使った！`），
/// 词典里没有以 `%1は%2` 开头的 key，凑出来就是 `%1は%2使用了！` 这种半日半中。
/// 所以这 34 条只能逐条给对照，key 就是游戏 `System.json` 里现有的原文。
///
/// 词典里真有这条 key 时以词典为准（`exact` 先查词典，这里只是兜底）。
fn message_template(key: &str) -> Option<&'static str> {
    Some(match key {
        // 经验 / 队伍名
        "現在の%1" => "当前经验 %1",
        "次の%1まで" => "距升级还差 %1",
        "%1たち" => "%1一行人",
        // 开战 / 收场
        "%1が出現！" => "%1出现了！",
        "%1は先手を取った！" => "%1抢得先机！",
        "%1は不意をつかれた！" => "%1遭到偷袭！",
        "%1は夢魔を無力化した" => "%1使梦魔失去了力量",
        "%1の体から力が抜けていく……" => "%1的身体逐渐失去力气……",
        // 获得
        "%1 の%2を得た" => "获得了%1的%2",
        "%1\\G 手に入れた！" => "获得了%1\\G！",
        "%1を手に入れた！" => "获得了%1！",
        "%1を覚えた！" => "学会了%1！",
        "%1は%2 %3 に上がった！" => "%1的%2提升到了%3！",
        "%1は%2を使った！" => "%1使用了%2！",
        // 伤害 / 恢复 / 增减（actor* 与 enemy* 里同文的合并成一条）
        "%1は精を奪われた！ %2 のダメージ！" => "%1被夺走了精气！受到 %2 点伤害！",
        "%1から魔力を奪った！ %2 のダメージ！" => "从%1身上夺走了魔力！造成 %2 点伤害！",
        "%1の%2が %3 回復した！" => "%1的%2恢复了 %3 点！",
        "%1の魔力が %3 回復した！" => "%1的魔力恢复了 %3 点！",
        "%1の%2が %3 増えた！" => "%1的%2增加了 %3 点！",
        "%1の%2が %3 減った！" => "%1的%2减少了 %3 点！",
        "%1は%2を %3 奪われた！" => "%1被夺走了 %3 点%2！",
        "%1の%2を %3 奪った！" => "夺走了%1的 %3 点%2！",
        "%1はダメージを受けていない！" => "%1没有受到伤害！",
        "ミス！　%1はダメージを受けていない！" => "失误！　%1没有受到伤害！",
        "%1にダメージを与えられない！" => "无法对%1造成伤害！",
        // 闪避 / 魔法 / 反击 / 掩护
        "%1は攻撃をかわした！" => "%1躲开了攻击！",
        "%1は魔法を打ち消した！" => "%1抵消了魔法！",
        "%1は魔法を跳ね返した！" => "%1反弹了魔法！",
        "%1の反撃！" => "%1的反击！",
        "%1が%2をかばった！" => "%1保护了%2！",
        // 强化 / 弱化
        "%1の%2が上がった！" => "%1的%2上升了！",
        "%1の%2が下がった！" => "%1的%2下降了！",
        "%1の%2が元に戻った！" => "%1的%2恢复了！",
        "%1には効かなかった！" => "对%1没有效果！",
        _ => return None,
    })
}

/// 读词典文件，原样返回「原文 → 译文」的表。
///
/// 不做 `\n` 摊平之类的加工：key 就是文件里那个样子，改一个字节都可能对不上。
pub fn load_index(path: &Path) -> Result<BTreeMap<String, String>> {
    let raw: String = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

/// 读取词典文件，转成内部统一模型 —— 给前端的词条列表用。
///
/// 之所以不直接把 `BTreeMap` 交出去：
/// MTool 的格式是「不由我们定义的外部契约」，一旦泄漏到上层，
/// 将来换格式就要动全工程。这里多做一步转换，把脏东西挡在外面。
pub fn load_ts_file(path: &Path) -> Result<Vec<TsSentence>> {
    Ok(load_index(path)?
        .into_iter()
        .map(|(source, target)| TsSentence { source, target })
        .collect())
}
