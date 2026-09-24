//! RGSS3 离线抽取：从 `Game.rgss3a`（或松散的 `Data/*.rvdata2`）里把原文抽成语料。
//!
//! 和 MV/MZ 那边（[`crate::corpus::offline`]）的关系：**产物一样，过程两样**。
//! 产物都是 [`Corpus`]，共用 `Granularity`、[`crate::corpus::is_worthy`] 和
//! 落盘格式；过程没有一处能共用 —— 那边读的是明文 JSON、按字段名取，
//! 这边要先解归档、再走 Ruby Marshal。硬要统一只能造「万能字节流遍历器」，
//! 那才是过度设计。
//!
//! 分派（哪个游戏走哪条）写在调用方 `task::collect_offline`，不在这里：
//! 否则 corpus 要反向 `use crate::engine`，转成环。

use std::path::Path;

use crate::corpus::rgss3::archive::DataSource;
use crate::corpus::rgss3::{rvdata2, scripts};
use crate::corpus::{Corpus, Granularity};
use crate::error::{Error, Result};

/// 数据库表。名字相对游戏根、用正斜杠（归档里是反斜杠，`DataSource` 统一过了）。
/// 照 `offline.rs` 的 `DB_TABLES` 惯例列成一张表，增删一行就行。
///
/// 这里没有地图：地图文件数多、名字不固定，靠 [`is_map_file`] 从归档里现挑。
const DB_FILES: &[&str] = &[
    "Data/MapInfos.rvdata2",
    "Data/CommonEvents.rvdata2",
    "Data/DataEx.rvdata2",
    "Data/Items.rvdata2",
    "Data/Skills.rvdata2",
    "Data/Weapons.rvdata2",
    "Data/Armors.rvdata2",
    "Data/States.rvdata2",
    "Data/Actors.rvdata2",
    "Data/Classes.rvdata2",
    "Data/Enemies.rvdata2",
    "Data/Troops.rvdata2",
    "Data/Tilesets.rvdata2",
    "Data/Animations.rvdata2",
    "Data/System.rvdata2",
];

/// `Data/Scripts.rvdata2` 装的是整个游戏的 Ruby 源码，单独走一条路（先解 zlib，
/// 再抠字符串字面量，见 [`scripts`]）。它不在 [`DB_FILES`] 里，因为那批文件统一
/// 走 Marshal 遍历，而这份文件的内容是压缩的 —— 混在一起会整段漏掉。
const SCRIPTS_FILE: &str = "Data/Scripts.rvdata2";

/// 扫一个 RGSS3 游戏目录，收成一份语料。
pub fn scan(game_dir: &Path) -> Result<Corpus> {
    let mut src = DataSource::open(game_dir)?;
    let mut c = Corpus::new();

    let mut files: Vec<String> = DB_FILES.iter().map(|s| s.to_string()).collect();
    files.extend(src.list_data().into_iter().filter(|n| is_map_file(n)));

    for name in files {
        // 这个游戏没有这张表（各引擎版本的表不齐），跳过不是错。
        if !src.contains(&name) {
            continue;
        }
        let raw = src.read(&name)?;
        // 带上文件名和开头字节：一看见 `04 08` 以外的开头，就知道这文件不是
        // 标准 Marshal，不用再猜是哪个文件、也不用再猜是不是解析器写错了。
        let texts = rvdata2::walk(&raw, &name).map_err(|e| {
            Error::new(format!("{name}（开头 {}）：{e}", hex_head(&raw)))
        })?;
        for t in texts {
            if is_noise(&t.path, &t.value) {
                continue;
            }
            take(&mut c, classify(&t.path, &t.value), &t.value);
        }
    }

    // 源码文案：和上面对称，但值是「源码里的字面量」，没有对象树路径，
    // 所以粒度和去噪都只能按文本自身判 —— 字面量只有单行/整段两种。
    if src.contains(SCRIPTS_FILE) {
        let raw = src.read(SCRIPTS_FILE)?;
        for lit in scripts::extract(&raw)? {
            take(&mut c, multi(&lit), &lit);
        }
    }

    Ok(c)
}

/// 这一条该不该丢掉 —— 丢的条件是「它不是画给玩家看的字」。
///
/// 判据不是拍脑袋来的：拿现成的汉化词典（562,595 条）当基准，按路径族逐族
/// 算命中率，真正的玩家文本命得很高（`@data.@name` 82%、`@display_name` 62%、
/// `@switches` 54%、`@terms.@commands` 79%），下面判掉的族全是 0~2.4%。
/// 命中率低不必然等于噪音（译者没覆盖也算 0），所以只判那些**另有理由**
/// 说明它不该翻的族 —— 文件名、公式、给脚本看的元数据，各自都讲得出道理。
///
/// 判不掉的还剩一块：`@list.@parameters` 里混着事件**注释**（指令 108/408）
/// 和 **Ruby 脚本**（355/655）。按指令码分才治本，但 [`rvdata2::walk`] 只吐
/// 字符串、`@code` 拿不到；好在脚本本身是纯 ASCII（`ssw(:op1)`、
/// `set_title_bg("Graphics/...")`、`!$game_player.normal_walk?`），
/// 早就被 [`crate::corpus::is_worthy`] 拦了 —— 实测这一族 3000 条里
/// 没有一条含 `end`/`#`/多行脚本。剩下的是日语注释，与真对话同形，留着。
fn is_noise(path: &str, text: &str) -> bool {
    if path.ends_with(NOTE_SUFFIX) {
        return true;
    }
    if NOISE_PATH_SUFFIXES.iter().any(|s| path.ends_with(s)) {
        return true;
    }
    is_tag_only(text)
}

/// RGSS3 原版引擎根本不读 `@note` —— 那是留给第三方脚本的元数据槽。
/// 本游戏（WF-RGSS）把设计注释、`<...>` 标签全塞在这里，实测 0% 命中。
/// `@learnings[n].@note` 也走这条。
const NOTE_SUFFIX: &str = ".@note";

/// 「值是文件名或公式」的 ivar。都带前导点写成整段后缀，
/// 免得 `.@name` 这种短后缀误伤玩家文本（`.@data.@name`）。
const NOISE_PATH_SUFFIXES: &[&str] = &[
    // 素材、音频文件名：运行时被当路径去 load，不是画出来的字。实测 0~2.4%。
    ".@character_name",
    ".@face_name",
    ".@battler_name",
    ".@animation1_name",
    ".@animation2_name",
    ".@parallax_name",
    ".@battleback1_name",
    ".@battleback2_name",
    ".@title1_name",
    ".@title2_name",
    ".@tileset_names",
    ".@bgm.@name",
    ".@bgs.@name",
    ".@se.@name",
    ".@me.@name",
    // 伤害公式：Ruby 表达式，运行时 eval。实测 0%。
    ".@damage.@formula",
    ".@damage.@enemy_formula",
];

/// 整条只有 `<...>` 标签（标签之间只剩空白），是写在注释里的元数据，
/// 不是给人读的句子。实测 65,148 条，占原始文本的 12.5%。
///
/// 标签**里**可以有真换行 —— 这里的 `text` 是解出来的原文，还带着真的 CR/LF，
/// 转义是落盘那一步（[`crate::dict::wtool::escape`]）才做的。
fn is_tag_only(text: &str) -> bool {
    let mut depth = 0usize;
    let mut seen = false;
    for ch in text.chars() {
        if depth > 0 {
            if ch == '>' {
                depth -= 1;
            }
            continue;
        }
        match ch {
            '<' => {
                depth = 1;
                seen = true;
            }
            ' ' | '\t' | '\r' | '\n' => {}
            _ => return false,
        }
    }
    seen && depth == 0
}

/// 出错时把开头几个字节打出来：一看见 `04 08` 以外的开头，就知道这个文件不是
/// 标准 Marshal，不用再猜是哪个文件、也不用再猜是不是解析器写错了。
fn hex_head(raw: &[u8]) -> String {
    raw.iter()
        .take(8)
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `Data/Map001.rvdata2` 这类地图文件。`MapInfos.rvdata2` 前缀也是 `Map`，
/// 但后面不是纯数字，会被挡掉。
fn is_map_file(name: &str) -> bool {
    let Some(stem) = name.rsplit('/').next().and_then(|f| f.strip_suffix(".rvdata2")) else {
        return false;
    };
    let Some(digits) = stem.strip_prefix("Map") else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit())
}

/// 按「这条文本在对象树里的位置」判粒度。
///
/// 粒度是**来源决定的属性**：同一条字符串，出现在事件指令参数里是「一行一条」，
/// 出现在 `@name` 里是菜单那种「整串画」。查表时的三级退级（整段 → 单行 →
/// 片段）就按这个来，判错粒度查表就对不上。
fn classify(path: &str, text: &str) -> Granularity {
    // 事件指令：`@list[n].@parameters[m]`（401/405 正文、101 说话人、102 选项）。
    if path.contains(".@list[") && path.contains(".@parameters[") {
        return multi(text);
    }
    // 名字：菜单、列表、地图名那一类，运行时是整串画的。
    if path.ends_with(".@name") {
        return Granularity::Fragment;
    }
    // 说明、注释、术语，以及其余一切：单行的按单行收，含换行的按整段收。
    multi(text)
}

fn multi(text: &str) -> Granularity {
    if text.contains('\n') {
        Granularity::Whole
    } else {
        Granularity::Line
    }
}

/// 收一条。整段的**同时**把它拆成单行各收一份 —— 和 MV 的 `take_list` 一个口径：
/// 整段是查表的第一个 key，拆不开时游戏会退到逐行查，行不在语料里就永远翻不上。
///
/// 每条还会再收一份「剥掉尾标点」的变体，理由见 [`TAIL_PUNCT`]。
fn take(c: &mut Corpus, g: Granularity, text: &str) {
    match g {
        Granularity::Whole => {
            take_one(c, Granularity::Whole, text);
            for line in text.split('\n') {
                // 源码里的换行常写成 `\r\n`（实测 WF-RGSS 的配置说明就是这么写的），
                // 照 `\n` 拆会让每行尾巴挂一个孤零零的 `\r` —— 那不是给玩家看的字。
                take_one(c, Granularity::Line, line.strip_suffix('\r').unwrap_or(line));
            }
        }
        g => take_one(c, g, text),
    }
}

/// 原样一条 + 剥尾标点一条（两者相同就不重复收）。
fn take_one(c: &mut Corpus, g: Granularity, text: &str) {
    c.push(g, text);
    let trimmed = text.trim_end_matches(TAIL_PUNCT);
    if trimmed.len() != text.len() {
        // 剥完可能太短（整条就是「あ。」），`Corpus::push` 里的 `is_worthy` 会挡住。
        c.push(g, trimmed);
    }
}

/// 绘制串的**尾标点**。这是语料和现成词典之间最大的一处**系统性**错位，
/// 不是哪张表漏扫。
///
/// 错位长什么样：词典的键一律不带尾标点（`それじゃ、あたしの部屋に来な。`
/// 查不到，`それじゃ、あたしの部屋に来な` 才查得到），而语料里存的是源数据/
/// 源码里**原样**的串，带 `。`。实测（`.probe_rgss3/gap2.py`）：不对齐这一层，
/// 562,595 条词典键只命中 10.8%；剥掉尾标点后命中 93.5%。
///
/// 所以两个形态都收：**原样**那条是游戏真正画出来的串（运行时精确查表要靠它），
/// **剥尾**那条对齐社区词典的键形态（用现成的汉化词典时靠它、以及片段匹配兜底）。
///
/// 字符集是按「实测各集合能救回多少未命中行」挑的（`.probe_rgss3/tail.py`）：
/// `。、！？…―」』）` 吃到 88.4%，再加半角 `. ! ? ~ ～` 到 89.3%；
/// **刻意不含空白** —— 加空格只多救 23 条，却会把整段的 `\n` 一起啃掉。
const TAIL_PUNCT: &[char] = &[
    '。', '、', '！', '？', '…', '―', '」', '』', '）', '.', '!', '?', '~', '～',
];

#[cfg(test)]
mod tests {
    use super::*;

    /// 事件指令参数判成单行，多行的判成整段。
    #[test]
    fn classifies_event_params() {
        let p = "Data/Map001.rvdata2.@events[1].@pages[0].@list[2].@parameters[0]";
        assert_eq!(classify(p, "こんにちは"), Granularity::Line);
        assert_eq!(classify(p, "一行目\n二行目"), Granularity::Whole);
    }

    /// `@name` 是整串画的片段；`@description` 跟正文一样按行/整段分。
    #[test]
    fn classifies_names_and_descriptions() {
        assert_eq!(
            classify("Data/Items.rvdata2[1].@name", "やくそう"),
            Granularity::Fragment
        );
        assert_eq!(
            classify("Data/Items.rvdata2[1].@description", "回復する\nHP+100"),
            Granularity::Whole
        );
    }

    /// 只挑 `MapNNN.rvdata2`，别把 `MapInfos.rvdata2` 当成地图。
    #[test]
    fn picks_map_files_only() {
        assert!(is_map_file("Data/Map001.rvdata2"));
        assert!(is_map_file("Data/Map/Data/Map042.rvdata2"));
        assert!(!is_map_file("Data/MapInfos.rvdata2"));
        assert!(!is_map_file("Data/Items.rvdata2"));
        assert!(!is_map_file("Data/Mapabc.rvdata2"));
    }

    /// 整段收进去时也拆了单行 —— 游戏退到逐行查时才有得查。
    #[test]
    fn whole_also_collects_lines() {
        let mut c = Corpus::new();
        take(&mut c, Granularity::Whole, "一行目\n二行目");
        assert_eq!(c.per_granularity(), (1, 2, 0));
    }

    /// `\r\n` 拆出来的行不带孤零零的 `\r`（源码里这么写的换行很常见）。
    #[test]
    fn strips_carriage_return_when_splitting_lines() {
        let mut c = Corpus::new();
        take(&mut c, Granularity::Whole, "一行目\r\n二行目\r\n");
        assert_eq!(c.per_granularity(), (1, 2, 0));
        assert!(c.flat().iter().any(|(_, t)| t == "一行目"));
        assert!(!c.flat().iter().any(|(_, t)| t.ends_with('\r')));
    }

    /// 带尾标点的收两条：原样（游戏真画的串）+ 剥掉尾标点的（社区词典的键形态）。
    /// 整段还要逐行各来一份，所以「一行目。\n二行目、」= 2 整段 + 4 单行。
    #[test]
    fn collects_tail_trimmed_variant_too() {
        let mut c = Corpus::new();
        take(&mut c, Granularity::Line, "それじゃ、あたしの部屋に来な。");
        assert_eq!(c.per_granularity(), (0, 2, 0));

        let mut c = Corpus::new();
        take(&mut c, Granularity::Whole, "一行目。\n二行目、");
        assert_eq!(c.per_granularity(), (2, 4, 0));

        // 不含尾标点的原样就一条，不重复收。
        let mut c = Corpus::new();
        take(&mut c, Granularity::Line, "こんにちは");
        assert_eq!(c.per_granularity(), (0, 1, 0));

        // 剥完只剩一个「あ」，不够格（`is_worthy` 要求 ≥2 字），只留原样那条。
        let mut c = Corpus::new();
        take(&mut c, Granularity::Line, "あ。");
        assert_eq!(c.per_granularity(), (0, 1, 0));

        // 只有**尾**标点被剥：开头的 `「` 和中间的 `、` 都不动。
        let mut c = Corpus::new();
        take(&mut c, Granularity::Line, "「そうか」");
        assert_eq!(c.per_granularity(), (0, 2, 0));
        assert!(c.flat().iter().any(|(_, t)| t == "「そうか"));
    }

    /// 元数据槽、素材文件名、伤害公式判成噪音。
    #[test]
    fn filters_metadata_assets_and_formulas() {
        let noisy = [
            "Data/Items.rvdata2[1].@note",
            "Data/Actors.rvdata2[1].@learnings[0].@note",
            "Data/Actors.rvdata2[1].@character_name",
            "Data/Map001.rvdata2.@events[1].@pages[0].@graphic.@face_name",
            "Data/Map001.rvdata2.@tileset_names",
            "Data/System.rvdata2.@bgm.@name",
            "Data/System.rvdata2.@se.@name",
            "Data/Skills.rvdata2[1].@damage.@formula",
            "Data/Skills.rvdata2[1].@damage.@enemy_formula",
        ];
        for p in noisy {
            assert!(is_noise(p, "スライム"), "{p} 该判成噪音");
        }
    }

    /// 玩家文本一条都不能误伤 —— 尤其 `.@name` 这种短后缀，
    /// `@character_name` 得靠前导点区分开。
    #[test]
    fn keeps_player_text() {
        let good = [
            "Data/Items.rvdata2[1].@name",
            "Data/Items.rvdata2[1].@description",
            "Data/Actors.rvdata2[1].@nickname",
            "Data/Map001.rvdata2.@events[1].@name",
            "Data/CommonEvents.rvdata2[1].@list[0].@parameters[0]",
            "Data/System.rvdata2.@terms.@commands[0]",
            "Data/Map001.rvdata2.@events[1].@pages[0].@list[0].@parameters[0]",
        ];
        for p in good {
            assert!(!is_noise(p, "やくそう"), "{p} 不该判成噪音");
        }
    }

    /// 整条只有 `<...>` 标签的丢掉；掺了别的字的留着。
    #[test]
    fn filters_tag_only_entries() {
        // 标签之间可以有空白和真换行。
        assert!(is_tag_only("<強化: 1.5>"));
        assert!(is_tag_only("<ターン: 3, 効果量: 200>\r\n"));
        assert!(is_tag_only("<a>\n<b:c>\n"));

        // 有标签也有正文 —— 正文是要翻的，留着。
        assert!(!is_tag_only("技の威力を上げる<強化: 1.5>"));
        assert!(!is_tag_only("こんにちは"));
        // 括号没配平、或者压根不是标签，都不算。
        assert!(!is_tag_only("<未閉じ"));
        assert!(!is_tag_only("a > b"));
        assert!(!is_tag_only("<tag> の後"));
    }
}