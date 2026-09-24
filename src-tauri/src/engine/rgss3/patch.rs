//! 用 NWPatch 把钩子架进 RGSS3 游戏 —— 零注入。
//!
//! VX Ace 的 `Game.exe` 启动时会 `File.read("Patch/Patch.rb")` 再 `eval` 它
//! （NWPatch 机制）。这条路不动进程内存、不落 DLL，杀软管不着，删文件即撤销。
//!
//! 干三件事：
//!
//! 1. 把 GUI 配的词典转成游戏侧能直接读的 TSV（`Patch/wtool_dict.tsv`）——
//!    RGSS3 是 Ruby 1.9.2、没有 `json`，所以不能直接喂 MTool 的 JSON；
//! 2. 把嵌进来的钩子写进 `Patch/wtool_hook.rb`；
//! 3. 在 `Patch/Patch.rb` 末尾追加一段 loader，并**重算第 1 行的认证号** ——
//!    NWPatch 会校验它，算错游戏直接 raise 不启动。
//!
//! 认证号算法（来自 Scripts.rvdata2 里 NWPatch 段的源码）：第 2 行起全文，
//! 去掉 ASCII 数字与 ASCII 空白后逐字符 Unicode 码点求和 = `n`；
//! `true_pass = n * (sqrt(n).to_s 的末位数字)`。
//!
//! **去数字去空白只能用 ASCII 判据**：Ruby 的 `\d` / `\s` 都只认 ASCII，
//! 写成 `char::is_numeric()` 会把全角数字也算进去，认证号就对不上了。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::dict;
use crate::engine::inject::{Mechanism, Report, Reversibility, State};
use crate::error::{Error, Result};

/// 钩子源码。编译期直接嵌进二进制（和 MZ 的桥同一个理由：这份文件是我们
/// 自己写的、不需要用户替换，嵌进来省掉运行时找路径的一堆事）。
const HOOK_RB: &str = include_str!("../../../resources/rgss3/wtool_hook.rb");

/// loader 段的起始标记。装/卸都按它定位，改认证号时不碰别的。
const MARKER: &str = "# ---- wtool RGSS3 hook loader (auto-generated) ----";

/// 钩子里那行字体常量（默认值）。用户在界面上选了别的字体就把整行换掉。
///
/// 为什么烤文件而不是像 MZ 那样运行时下发：RGSS3 这条路的代码是游戏启动时
/// 从盘上 `eval` 的，wtool 和游戏之间没有连接通道，改字体只能改文件。
const FONT_LINE: &str = "FONT_NAME = \"黑体\"";

/// 追加到 `Patch.rb` 末尾的 loader。纯 ASCII，不放日文 —— 认证号是按字面
/// 内容算的，写起来越简单越不容易算错。
const LOADER: &str = r##"
# ---- wtool RGSS3 hook loader (auto-generated) ----
if File.exist?("Patch/wtool_hook.rb")
  begin
    _wt_src = File.open("Patch/wtool_hook.rb", "r:utf-8") { |_wt_f| _wt_f.read }
    eval(_wt_src, TOPLEVEL_BINDING, "Patch/wtool_hook.rb")
  rescue Exception => _wt_e
    begin
      File.open("Patch/wtool_hook.err", "w:utf-8") do |_wt_f|
        _wt_f.puts("#{_wt_e.class}: #{_wt_e.message}")
        _wt_f.puts(_wt_e.backtrace)
      end
    rescue Exception
    end
  end
end
# ---- wtool end ----
"##;

/// 「写 Patch.rb」这条注入路。
pub struct Rgss3Patch;

impl Mechanism for Rgss3Patch {
    fn id(&self) -> &'static str {
        "rgss3_patch"
    }

    fn name(&self) -> &'static str {
        "RGSS3 Patch.rb（零注入）"
    }

    /// 只看磁盘。找不到 `Patch/Patch.rb` 就是「这条路在这游戏上不成立」。
    fn probe(&self, game_dir: &Path) -> Report {
        let dir = game_dir.join("Patch");
        if !dir.join("Patch.rb").is_file() {
            return Report {
                state: State::NotApplicable,
                detail: "没有 Patch/Patch.rb，用不了 NWPatch 这条路".to_string(),
            };
        }

        let hook = dir.join("wtool_hook.rb");
        match std::fs::read_to_string(&hook) {
            Err(_) => Report {
                state: State::Ready,
                detail: "还没装，可以安装".to_string(),
            },
            // 比的是「去掉字体那行之后」的内容：字体是用户选的，不该影响
            // 「这份钩子是不是当前版本」的判断（否则换个字体就报「需重装」）。
            Ok(rb) if strip_font_line(&rb) != strip_font_line(HOOK_RB) => Report {
                state: State::Stale,
                detail: "钩子不是当前版本，需要重装".to_string(),
            },
            Ok(_) => {
                let patch = std::fs::read_to_string(dir.join("Patch.rb")).unwrap_or_default();
                if !patch.contains(MARKER) {
                    Report {
                        state: State::Stale,
                        detail: "钩子在，但 Patch.rb 里没挂 loader，游戏不会加载，需要重装"
                            .to_string(),
                    }
                } else if !dir.join("wtool_dict.tsv").is_file() {
                    Report {
                        state: State::Stale,
                        detail: "词典文件不在，需要重装".to_string(),
                    }
                } else {
                    Report {
                        state: State::Installed,
                        detail: "已安装，游戏下次启动时加载".to_string(),
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

    /// 盘上多了一串文件、`Patch.rb` 也被改过，但都能靠备份或就地摘除还回去。
    fn reversibility(&self) -> Reversibility {
        Reversibility::RuntimeAndOffline
    }

    /// `Patch/Patch.rb` 是游戏启动时读的，改了得重启游戏才生效。
    fn restart_required(&self) -> bool {
        true
    }
}

/// 装钩子，返回几句给用户看的话。
fn install(game_dir: &Path, dict_path: &Path, font: &str) -> Result<Vec<String>> {
    let dir = find_patch_dir(game_dir)?;
    let mut log = Vec::new();

    // RGSS3 的查词全在游戏进程里做，词典必须随安装投进去 —— 没配就装不了。
    if dict_path.as_os_str().is_empty() {
        return Err(Error::new(
            "RGSS3 这条路要把词典投进游戏目录，请先在界面上选好词典文件",
        ));
    }

    // ---- 1. 词典：GUI 配的那个 → 游戏侧能直接读的 TSV ----
    let map = dict::load_map(dict_path)?;
    let (tsv, stats) = build_tsv(&map, dict_path);
    let dict_dst = dir.join("wtool_dict.tsv");
    write_atomic(&dict_dst, &tsv)?;
    log.push(format!(
        "词典 {} 条 → {}（跳过 纯 ASCII {}、单字 {}、空译文 {}、含换行/制表 {}）",
        stats.kept,
        dict_dst.display(),
        stats.ascii,
        stats.short,
        stats.empty,
        stats.ctl,
    ));

    // ---- 2. 钩子本体（用户选的字体烤进 FONT_NAME 那行）----
    let hook_src = build_hook(font);
    let hook = dir.join("wtool_hook.rb");
    if std::fs::read_to_string(&hook).ok().as_deref() == Some(hook_src.as_str()) {
        log.push("wtool_hook.rb 已是最新".to_string());
    } else {
        write_atomic(&hook, &hook_src)?;
        log.push(format!("已写入 {}", hook.display()));
        if !font.trim().is_empty() {
            log.push(format!("字体：{}（游戏重启后生效）", font.trim()));
        }
    }

    // ---- 3. Patch.rb：备份 + 追加 loader + 重算认证号 ----
    let rb = dir.join("Patch.rb");
    let src = read_utf8(&rb)?;

    // 备份只留第一份 —— 保留的必须是用户原始的那份，不是我们改过的。
    let bak = dir.join("Patch.rb.wtool-bak");
    if !bak.exists() {
        std::fs::write(&bak, &src)?;
        log.push(format!("原 Patch.rb 已备份到 {}", bak.display()));
    }

    let appended = if src.contains(MARKER) {
        log.push("Patch.rb：loader 已在，只重算认证号".to_string());
        src.clone()
    } else {
        let mut s = src.clone();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(LOADER);
        log.push("Patch.rb：已追加 loader".to_string());
        s
    };

    let (out, tp) = rewrite_auth(&appended)?;
    if out != src {
        write_atomic(&rb, &out)?;
    }
    log.push(format!("Patch.rb 认证号 → # {tp}"));
    log.push("游戏如果已经开着，要重启才会加载钩子".to_string());

    Ok(log)
}

/// 卸钩子，返回几句给用户看的话。
///
/// 有备份就整文件还原（认证号随之回到原值）；没有就**就地**把 loader 段摘掉
/// 再重算认证号 —— 用户自己改过 `Patch.rb` 时只能这么办，不能拿旧备份覆盖。
fn uninstall(game_dir: &Path) -> Result<Vec<String>> {
    let dir = find_patch_dir(game_dir)?;
    let mut log = Vec::new();

    let rb = dir.join("Patch.rb");
    let bak = dir.join("Patch.rb.wtool-bak");

    if bak.exists() {
        let text = read_utf8(&bak)?;
        write_atomic(&rb, &text)?;
        std::fs::remove_file(&bak)?;
        log.push("Patch.rb 已还原到备份（认证号回到原值）".to_string());
    } else if rb.exists() {
        let text = read_utf8(&rb)?;
        if let Some(at) = text.find(MARKER) {
            // loader 是我们追加的：前面会多出一个空行，一并去掉，
            // 让摘完的正文和当初逐字节一致（和 Python 版同一套处理）。
            let head = format!("{}\n", text[..at].trim_end_matches('\n'));
            let (out, tp) = rewrite_auth(&head)?;
            write_atomic(&rb, &out)?;
            log.push(format!("没有备份，已就地摘掉 loader 并重算认证号（# {tp}）"));
        } else {
            log.push("Patch.rb 里没有 loader，未改动".to_string());
        }
    } else {
        log.push("Patch.rb 不存在，跳过".to_string());
    }

    // 钩子/词典/日志/错误文件/未命中记录一起清。日志可能被正跑着的游戏占着，
    // 删不掉不影响游戏，交给用户手动处理。
    for name in [
        "wtool_hook.rb",
        "wtool_dict.tsv",
        "wtool_hook.log",
        "wtool_hook.err",
        "wtool_miss.log",
    ] {
        let p = dir.join(name);
        if !p.exists() {
            continue;
        }
        match std::fs::remove_file(&p) {
            Ok(()) => log.push(format!("已删除 {}", p.display())),
            Err(e) => log.push(format!("删不掉 {}（{e}），不影响游戏，可手动删除", p.display())),
        }
    }

    log.push("游戏如果正开着，要重启才会真的断开".to_string());
    Ok(log)
}

/// RGSS3 的 `Patch/` 只在游戏根目录，不试 `www/`（那是 MV 的封装）。
fn find_patch_dir(game_dir: &Path) -> Result<PathBuf> {
    let dir = game_dir.join("Patch");
    if dir.join("Patch.rb").is_file() {
        return Ok(dir);
    }
    Err(Error::new(format!(
        "{} 里找不到 Patch/Patch.rb，这不是一个用 NWPatch 的 RGSS3 游戏，或者游戏目录填错了？",
        game_dir.display()
    )))
}

/// 把用户选的字体写进钩子源码的 `FONT_NAME` 那行。
///
/// 空串 = 不动，用钩子自带的那条默认字体。名字里的引号和反斜杠要转义 ——
/// 界面上那个名字是能手打的，不转义就能把整份 Ruby 拆坏、游戏起不来。
fn build_hook(font: &str) -> String {
    let font = font.trim();
    if font.is_empty() {
        return HOOK_RB.to_string();
    }
    let escaped = font.replace('\\', "\\\\").replace('"', "\\\"");
    HOOK_RB.replace(FONT_LINE, &format!("FONT_NAME = \"{escaped}\""))
}

/// 比对「盘上这份钩子是不是当前版本」之前，先把字体那行抹成固定形状。
///
/// 字体是用户选的，不该让版本比对跟着它变 —— 否则换个字体就被判成「需重装」，
/// 而用户其实什么都没坏。
fn strip_font_line(src: &str) -> String {
    src.lines()
        .map(|l| {
            if l.trim_start().starts_with("FONT_NAME") {
                "FONT_NAME = ?"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 读文件按 UTF-8，读不出来给一句能看懂的话。
///
/// `Patch.rb` 实测是 UTF-8；不是的话**拒绝改写** —— 改坏了游戏起不来。
fn read_utf8(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| {
        Error::new(format!(
            "读不了 {}（{e}）—— 这个文件必须是 UTF-8，拒绝改写以免游戏起不来",
            path.display()
        ))
    })
}

/// 词典统计，装完报一行给用户看。
struct DictStats {
    kept: usize,
    ascii: usize,
    short: usize,
    empty: usize,
    ctl: usize,
}

/// 把词典转成 TSV 正文（UTF-8，每行「原文<TAB>译文」，`#` 开头是注释）。
///
/// 过滤的条目在钩子里本来就到不了查表那一步：
///
/// - **纯 ASCII key**：钩子有「含假名/汉字」守卫，纯 ASCII 根本不查；
/// - **单字 key**：`lookup` 的长度守卫挡掉（单字查表正是文本错位的成因）；
/// - **空译文**：没有意义；
/// - **含真换行/制表的条目**：没法规整地塞进行式 TSV，留着会撑破格式。
fn build_tsv(map: &BTreeMap<String, String>, src: &Path) -> (String, DictStats) {
    let mut stats = DictStats {
        kept: 0,
        ascii: 0,
        short: 0,
        empty: 0,
        ctl: 0,
    };
    let name = src
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut out = format!("# wtool-dict 1\tfrom\t{name}\n");
    for (k, v) in map {
        if k.is_empty() || v.is_empty() {
            stats.empty += 1;
            continue;
        }
        if k.contains(['\t', '\n', '\r']) || v.contains(['\t', '\n', '\r']) {
            stats.ctl += 1;
            continue;
        }
        if !has_kana_or_kanji(k) {
            stats.ascii += 1;
            continue;
        }
        // 长度按「字符数」算，与 Ruby 的 `String#length` 对齐（都是码点个数）。
        if k.chars().count() < 2 {
            stats.short += 1;
            continue;
        }
        out.push_str(k);
        out.push('\t');
        out.push_str(v);
        out.push('\n');
        stats.kept += 1;
    }

    (out, stats)
}

/// 含假名或汉字才算「可能要翻」。和钩子里 `NEEDS_TR` 的范围保持一致。
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

/// 复现 NWPatch 的认证号，并把第 1 行重写成 `# <认证号>`。
/// 返回（新正文，认证号）。
fn rewrite_auth(text: &str) -> Result<(String, u64)> {
    let (tp, _n, _root) = auth_number(text)?;

    // 第 1 行是 `# <数字>`，纯 ASCII，可以按字节切。
    let first_end = text.find('\n').map(|i| i + 1).unwrap_or(text.len());
    let eol = if text[..first_end].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = format!("# {tp}{eol}");
    out.push_str(&text[first_end..]);
    Ok((out, tp))
}

/// 认证号：返回（认证号, 基数和 n, sqrt 的字符串形式）。
fn auth_number(text: &str) -> Result<(u64, u64, String)> {
    let n: u64 = text
        .split('\n')
        .skip(1) // 第 1 行是认证号本身，不参与
        .flat_map(str::chars)
        .filter(|c| !is_ascii_digit_or_ws(*c))
        .map(|c| c as u64)
        .sum();

    if n == 0 {
        return Err(Error::new("Patch.rb 认证号基数算出 0，内容异常，拒绝改写"));
    }

    // `Math.sqrt(n).to_s` 的末位数字。Ruby/Python 对整数开方会写成 `4.0`
    // （带小数点），Rust 的 `f64` Display 写成 `4` —— 补上小数点才对得上。
    let mut root = (n as f64).sqrt().to_string();
    if !root.contains('.') {
        root.push_str(".0");
    }
    let last = root
        .chars()
        .last()
        .and_then(|c| c.to_digit(10))
        .unwrap_or(0);

    Ok((n * last as u64, n, root))
}

/// Ruby 的 `\d` / `\s` 只认这 6 个 ASCII 空白 + 0-9，别用 Unicode 判据。
fn is_ascii_digit_or_ws(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0C' | '\x0B')
}

/// 先写同目录的 `.tmp` 再 rename（理由同 MZ 桥：改坏 `Patch.rb` 游戏起不来）。
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".wtool.tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, content)?;
    // Windows 上 rename 到已存在的文件会失败，先删目标。窗口极小，
    // 而且此时 `.tmp` 已经完整落盘。
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本项目一贯不引 `tempfile`（连 `link.rs` 都只用 std）。
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wtool_rgss3_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Patch")).unwrap();
        dir
    }

    /// 一份「原样」的 `Patch.rb`：第 1 行是认证号，后面几行是有效载荷。
    const PATCH: &str = "# 1861496\n# encoding: utf-8\nputs \"hello\"\n";

    #[test]
    fn auth_number_strips_only_ascii_digits_and_ws() {
        // 第 2 行是「全角２ + 全角空格 + ASCII 空格」：只有那个 ASCII 空格
        // 该被抹掉，全角两个字符是有效载荷，要和它们的码点。
        let (_, n, _) = auth_number("# 0\n２\u{3000} ").unwrap();
        assert_eq!(n, 0xFF12 + 0x3000); // 全角２ + 全角空格

        // 整数开方 Ruby 写成 "2.0"，末位是 0 —— 认证号必须是 0。
        // 若照 Rust 的 f64 Display（"2"）取末位就会得到 8，是错的。
        let (tp, _, _) = auth_number("# 0\n\u{4}").unwrap(); // 载荷码点 4，√4=2.0
        assert_eq!(tp, 0);
    }

    #[test]
    fn roundtrip_is_byte_exact() {
        let root = scratch("roundtrip");
        let rb = root.join("Patch").join("Patch.rb");
        std::fs::write(&rb, PATCH).unwrap();
        // 一份最小词典
        let dict = root.join("dict.json");
        std::fs::write(&dict, "{\"石化能力\":\"石化能力\"}").unwrap();

        install(&root, &dict, "").unwrap();
        let installed = std::fs::read_to_string(&rb).unwrap();
        assert!(installed.contains(MARKER));
        assert!(installed.starts_with("# "));
        assert!(root.join("Patch").join("wtool_hook.rb").is_file());

        uninstall(&root).unwrap();
        assert_eq!(std::fs::read_to_string(&rb).unwrap(), PATCH);
        assert!(!root.join("Patch").join("wtool_hook.rb").exists());
        assert!(!root.join("Patch").join("Patch.rb.wtool-bak").exists());
    }

    /// 选了字体就烤进那行；不选就保持默认。两边的「版本比对」都该判成当前版本
    /// —— 换个字体不该被误判成「需重装」。
    #[test]
    fn font_is_baked_in_but_does_not_affect_version_check() {
        let root = scratch("font");
        let rb = root.join("Patch").join("Patch.rb");
        std::fs::write(&rb, PATCH).unwrap();
        let dict = root.join("dict.json");
        std::fs::write(&dict, "{\"石化能力\":\"石化能力\"}").unwrap();

        let hook = root.join("Patch").join("wtool_hook.rb");

        // 不指定字体：盘上那份和源码逐字节相同。
        install(&root, &dict, "").unwrap();
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), HOOK_RB);

        // 指定字体：那行被换掉，其余逐字节不变。
        install(&root, &dict, "微软雅黑").unwrap();
        let text = std::fs::read_to_string(&hook).unwrap();
        assert!(text.contains("FONT_NAME = \"微软雅黑\""));
        assert_eq!(text.replace("微软雅黑", "黑体"), HOOK_RB);
        assert!(Rgss3Patch.probe(&root).state == State::Installed);

        // 空串装回来：又回到默认那份。
        install(&root, &dict, "").unwrap();
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), HOOK_RB);
        assert!(Rgss3Patch.probe(&root).state == State::Installed);
    }

    #[test]
    fn tsv_skips_single_char_and_ascii_keys() {
        let mut map = BTreeMap::new();
        map.insert("石化能力".to_string(), "石化能力а".to_string());
        map.insert("HP".to_string(), "生命".to_string()); // 纯 ASCII key
        map.insert("あ".to_string(), "啊".to_string()); // 单字 key
        map.insert("たべる".to_string(), String::new()); // 空译文
        let (tsv, stats) = build_tsv(&map, Path::new("x.json"));
        assert_eq!(stats.kept, 1);
        assert_eq!(stats.ascii, 1);
        assert_eq!(stats.short, 1);
        assert_eq!(stats.empty, 1);
        assert!(tsv.contains("石化能力\t"));
        assert!(!tsv.contains("HP\t"));
    }
}