//! 把桥插件装进游戏目录 —— 让软件自己干，而不是让用户去手动放文件、改 `plugins.js`。
//!
//! 这是 wtool 现在**唯一的一条注入方式**（见 `engine::inject::Mechanism`）。
//! 它有两个前提，缺一不可：引擎自己有插件机制，而且跑在能执行 JS 的运行时上。
//!
//! 干两件事：
//!
//! 1. 把 `wtool_bridge.js` 写进 `<游戏根>/js/plugins/`；
//! 2. 在 `<游戏根>/js/plugins.js` 的 `$plugins` 数组里把 `wtool_bridge` 打开，
//!    顺手把 `wtool_reader`（同样是钩 `Window_Message` 的探测插件）关掉——
//!    两个一起开会挂两次钩子，同一段对话上报两遍。
//!
//! 安装是幂等的：内容一样就不碰盘，改一次留一份备份。
//! `uninstall` 是它的逆运算，那个函数上面写着为什么不能简单地「删掉那一行」。

use std::path::{Path, PathBuf};

use crate::engine::inject::{Mechanism, Mode, Report, Reversibility, Service, State};
use crate::error::{Error, Result};

/// 桥的源码。编译期直接嵌进二进制。
///
/// 不用 `bundle.resources` + 运行时找路径，是因为那样光是「开发时资源在
/// `target/debug` 里还是在 `resources/` 里」就够折腾了，而这份文件是
/// 我们自己写的、不需要用户替换，嵌进去最省事。
pub(crate) const BRIDGE_JS: &str = include_str!("../../../resources/mz_mv/wtool_bridge.js");

/// 插件源码里那两行「一次性载荷」占位。安装器（`mz::baked`）把它们整行换掉：
///
/// - `var WTOOL_BAKED = null;` → 烤入的词典对象字面量；
/// - `var WTOOL_BAKED_FONT = '';` → 烤入的字体名。
///
/// **这两行在源码里必须各出现一次，逐字节一致**（含 4 空格缩进）——替换是纯
/// 字符串操作，差一个空格就换不中。改插件源码时别顺手格式化它们。
pub(crate) const BAKED_DICT_LINE: &str = "    var WTOOL_BAKED = null;";

/// 字体那行占位，见 [`BAKED_DICT_LINE`]。
pub(crate) const BAKED_FONT_LINE: &str = "    var WTOOL_BAKED_FONT = '';";

/// 桥在 `$plugins` 里的注册名。必须和文件名一致，RPG Maker 是按名字找文件的。
const BRIDGE_NAME: &str = "wtool_bridge";

/// 只负责探测、不负责翻译的那个插件。桥自己会钩同一处，所以装桥时把它关掉。
const PROBE_NAME: &str = "wtool_reader";

/// 一次性模式的烤入载荷（`mz::baked`）写进插件源码后，`WTOOL_BAKED` 那行的开头。
///
/// 插件原文里这行是 `var WTOOL_BAKED = null;`，烤入时被整行换成对象字面量，
/// 于是以 `var WTOOL_BAKED = {` 开头。拿它当「这份文件是我们写的（实时或一次性
/// 任一）」的判据 —— 两条路写的是同一个文件，卸载时都得认。
pub(crate) const BAKED_PREFIX: &str = "var WTOOL_BAKED = {";

/// 盘上这份 `wtool_bridge.js` 是不是 wtool 写的（实时源码，或带烤入载荷）。
fn is_our_file(js: &str) -> bool {
    js == BRIDGE_JS || js.contains(BAKED_PREFIX)
}

/// 「写 js/plugins」这条注入路（实时模式）。
pub struct JsPluginBridge;

impl Mechanism for JsPluginBridge {
    fn id(&self) -> &'static str {
        "js_plugin_live"
    }

    fn name(&self) -> &'static str {
        "JS 插件桥（实时，连 wtool）"
    }

    fn mode(&self) -> Mode {
        Mode::Live
    }

    /// 实时这条要靠 wtool 的两个端口活着：主通道 17989 + 同步查表 17990。
    fn service(&self) -> Service {
        Service::Tcp
    }

    /// 只看磁盘。目录填错、文件没权限都算的一种「探测结果」，不报错。
    fn probe(&self, game_dir: &Path) -> Report {
        probe_js(game_dir, |js| js == BRIDGE_JS)
    }

    /// 词典和字体都不走盘：词典是桥连上后由 wtool 逐条回写，字体是连上后经
    /// 内存通道下发的，所以这两个参数这里都用不上。
    fn install(&self, game_dir: &Path, _dict_path: &Path, _font: &str) -> Result<Vec<String>> {
        install_with(game_dir, BRIDGE_JS)
    }

    fn uninstall(&self, game_dir: &Path) -> Result<Vec<String>> {
        uninstall(game_dir)
    }

    /// 盘上动了两个文件，但都能靠备份还回去，所以是「运行时禁用 + 离线恢复」。
    fn reversibility(&self) -> Reversibility {
        Reversibility::RuntimeAndOffline
    }

    /// `plugins.js` 是启动时读的，改了得重启游戏才生效。
    fn restart_required(&self) -> bool {
        true
    }
}

/// 只看磁盘判断「这条 JS 插件路现在什么状态」。
///
/// `current` 判断盘上那份是不是本方式写的当前版本：实时看「和源码逐字节相同」，
/// 一次性看「带烤入载荷」。两条路写同一个文件，所以探测逻辑共用，见
/// [`probe`](JsPluginBridge::probe) 与 `mz::baked`。
pub(crate) fn probe_js(game_dir: &Path, current: impl Fn(&str) -> bool) -> Report {
    let root = match find_root(game_dir) {
        Ok(root) => root,
        Err(e) => {
            return Report {
                state: State::NotApplicable,
                detail: e.message,
            }
        }
    };

    let bridge = root.join("js").join("plugins").join("wtool_bridge.js");
    let list = std::fs::read_to_string(root.join("js").join("plugins.js")).unwrap_or_default();
    let entry = list.lines().find(|l| is_entry_named(l, BRIDGE_NAME));

    match std::fs::read_to_string(&bridge) {
        Err(_) => Report {
            state: State::Ready,
            detail: "还没装，可以安装".to_string(),
        },
        // 注册表里没有，游戏压根不会加载这个文件 —— 和没装一样没用。
        Ok(_) if entry.is_none() => Report {
            state: State::Stale,
            detail: "插件文件在，但 plugins.js 里没打开它，游戏不会加载，需要重装".to_string(),
        },
        // `disable_status` 认得出「本来是 true」才会返回 Some；
        // 返回 None 就说明它现在是关着的。
        Ok(_) if entry.is_some_and(|l| disable_status(l).is_none()) => Report {
            state: State::Stale,
            detail: "插件在，但在 plugins.js 里被关掉了，需要重装".to_string(),
        },
        Ok(js) if !current(&js) => Report {
            state: State::Stale,
            detail: "插件内容不是当前版本，需要重装".to_string(),
        },
        Ok(_) => Report {
            state: State::Installed,
            detail: "已安装，并在 plugins.js 里打开".to_string(),
        },
    }
}

/// 装桥，返回几句给用户看的话。
///
/// `content` 是插件本体的正文：实时模式给 [`BRIDGE_JS`]，一次性模式给烤入词典
/// 与字体之后的那份（见 `mz::baked`）。两条路除了正文不同，落盘和改
/// `plugins.js` 的动作完全一样，所以共用这一段。
///
/// 游戏目录来自 GUI，用户在别处填错了这里会直接报错，不会瞎写。
pub(crate) fn install_with(game_dir: &Path, content: &str) -> Result<Vec<String>> {
    let root = find_root(game_dir)?;
    let mut log = Vec::new();

    // ---- 1. 插件本体 ----
    let bridge_path = root.join("js").join("plugins").join("wtool_bridge.js");
    if std::fs::read_to_string(&bridge_path).ok().as_deref() == Some(content) {
        log.push("wtool_bridge.js 已是最新".to_string());
    } else {
        write_atomic(&bridge_path, content)?;
        log.push(format!("已写入 {}", bridge_path.display()));
    }

    // ---- 2. 注册表 ----
    let list_path = root.join("js").join("plugins.js");
    let src = std::fs::read_to_string(&list_path)?;
    let (patched, changed) = patch(&src)?;

    if changed {
        // 备份只在真的要改的时候留，且只留第一份——保留的是用户原始的那份。
        let backup = root.join("js").join("plugins.js.wtool.bak");
        if !backup.exists() {
            std::fs::write(&backup, &src)?;
            log.push(format!("原 plugins.js 已备份到 {}", backup.display()));
        }
        write_atomic(&list_path, &patched)?;
        log.push("plugins.js 已更新".to_string());
        log.push("游戏如果已经开着，要重启才会加载桥".to_string());
    } else {
        log.push("plugins.js 无需改动".to_string());
    }

    Ok(log)
}

/// 卸桥，返回几句给用户看的话。
///
/// 比装麻烦，因为「撤回去」有几条路，选错了会伤到用户的东西：
///
/// 1. **顺序不能反。** 先写成功 `plugins.js`，最后才删插件文件。反过来的话，
///    万一写盘失败，游戏目录里就留下「注册表里有这条、文件却没了」——
///    MZ 加载不到插件会直接抛错，游戏起不来。
/// 2. **备份只能用来「做证明」，不能直接覆盖。** RPG Maker 的编辑器每次保存
///    都会整个重写 `plugins.js`，用户在装了桥之后自己加过插件是常态。所以只有
///    把备份重新装一遍、结果和当前文件逐字节相同（= 这中间只有我们改过），
///    才敢整文件还原；否则只撤我们加的那部分。
/// 3. 插件文件只在「确实是 wtool 写的」时才删（实时源码或带烤入载荷）：
///    用户改过就留着，那是他的东西，删了不可挽回。
pub(crate) fn uninstall(game_dir: &Path) -> Result<Vec<String>> {
    let root = find_root(game_dir)?;
    let mut log = Vec::new();

    let list_path = root.join("js").join("plugins.js");
    let src = std::fs::read_to_string(&list_path)?;
    let backup = std::fs::read_to_string(root.join("js").join("plugins.js.wtool.bak")).ok();

    // 「把备份重新装一遍能得到当前文件」= 从备份到现在只有我们动过手。
    let restore = backup
        .as_deref()
        .filter(|bak| matches!(patch(bak), Ok((p, _)) if p == src));

    let out = match restore {
        Some(bak) => {
            log.push("plugins.js 已还原到备份（确认这中间只有 wtool 改过）".to_string());
            bak.to_string()
        }
        None => {
            let (cleaned, changed) = unpatch(&src);
            if changed {
                log.push(
                    "plugins.js 已移除 wtool_bridge（你之后又改过 plugins.js，所以只撤了 wtool 加的那部分）"
                        .to_string(),
                );
                // 只撤自己的改动时，`wtool_reader` 不再自动还原：判断「当初是不是
                // 我们关的」得去解析备份，而它也是 wtool 的插件，留着关掉不影响游戏，
                // 不值得为它引入一套双向 status 改写。
                if src.lines().any(|l| is_entry_named(l, PROBE_NAME)) {
                    log.push("wtool_reader 仍是关闭状态（它也是 wtool 的插件，不影响游戏）".to_string());
                }
            } else {
                log.push("plugins.js 里没有 wtool_bridge，无需改动".to_string());
            }
            cleaned
        }
    };

    if out != src {
        write_atomic(&list_path, &out)?;
    }

    // 备份的使命就是「把 plugins.js 还回原样」。既然已经完整还原（当前内容
    // 和它逐字节相同），它就完成使命了 —— 留着只是让游戏目录里多一个用户
    // 看不懂的文件。只在真的还原过时才删，只撤一部分时还留着（那时候它仍是
    // 「用户原始 plugins.js」的唯一记录）。
    if restore.is_some() {
        let bak = root.join("js").join("plugins.js.wtool.bak");
        if std::fs::remove_file(&bak).is_ok() {
            log.push("备份文件已清理（已经用不上了）".to_string());
        }
    }

    // ---- 插件文件最后删 ----
    let bridge_path = root.join("js").join("plugins").join("wtool_bridge.js");
    match std::fs::read_to_string(&bridge_path) {
        Ok(js) if is_our_file(&js) => match std::fs::remove_file(&bridge_path) {
            Ok(()) => log.push(format!("已删除 {}", bridge_path.display())),
            // 杀软或者编辑器占着删不掉：不影响游戏，交给用户手动处理。
            Err(e) => log.push(format!(
                "插件文件删不掉（{e}），不影响游戏，可以手动删除 {}",
                bridge_path.display()
            )),
        },
        Ok(_) => log.push("wtool_bridge.js 被改过，保留没删".to_string()),
        Err(_) => log.push("wtool_bridge.js 本来就不在".to_string()),
    }

    // ---- 顺手清掉桥写下的运行日志 ----
    // 这些日志是桥在游戏侧自己落的，解铃还须系铃人：用户点「取消注入」意味着
    // 「把我装的东西都撤走」，留一堆 wtool.log 在游戏目录里不符合这个预期。
    log.extend(clean_logs(&root));

    log.push("游戏如果正开着，要重启才会真的断开".to_string());
    Ok(log)
}

/// 删掉桥写在游戏目录里的运行日志（`wtool.log` 与轮转的 `wtool.1-4.log`）。
///
/// 位置和 `wtool_bridge.js` 里的落盘逻辑一致：优先游戏根目录，退而求其次是
/// `js/plugins/`（桥找不到可写根目录时会落到插件自己那层）。两处都扫一遍，
/// 删不掉就记一句、不报错 —— 日志不是游戏资源，删不掉也不影响撤离是否成功。
fn clean_logs(root: &Path) -> Vec<String> {
    // `wtool.log` + `wtool.1.log` … `wtool.4.log`，与插件的 `LOG_KEEP = 5` 对应。
    let names = [
        "wtool.log",
        "wtool.1.log",
        "wtool.2.log",
        "wtool.3.log",
        "wtool.4.log",
    ];

    let mut removed = 0usize;
    let mut failed = Vec::new();
    for dir in [root.to_path_buf(), root.join("js").join("plugins")] {
        for name in names {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) => failed.push(format!("{}（{e}）", path.display())),
            }
        }
    }

    let mut log = Vec::new();
    if removed > 0 {
        log.push(format!("已清理 {removed} 个 wtool 运行日志"));
    }
    for f in failed {
        log.push(format!("日志没删掉：{f}，可以手动删除"));
    }
    log
}

/// MV 把整个游戏塞在 `www/` 下，MZ 直接铺在根目录，两个都试一遍。
pub(crate) fn find_root(game_dir: &Path) -> Result<PathBuf> {
    for root in [game_dir.to_path_buf(), game_dir.join("www")] {
        if root.join("js").join("plugins").is_dir() {
            return Ok(root);
        }
    }
    Err(Error::new(format!(
        "{} 里找不到 js/plugins 目录，游戏目录是不是填错了？",
        game_dir.display()
    )))
}

/// 改 `plugins.js` 的正文。返回（新正文，是否真的改动了什么）。
fn patch(src: &str) -> Result<(String, bool)> {
    // RPG Maker 在 Windows 上写出来的可能是 CRLF，原样还回去，
    // 免得为了改一行把整个文件的换行符翻一遍。
    let eol = if src.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
    let mut changed = false;

    // ---- 打开桥 ----
    // 先定位桥那一行：已经有了就是它自己，没有就把新行插在 `];` 前面。
    let at = match lines.iter().position(|l| is_entry_named(l, BRIDGE_NAME)) {
        Some(i) => i,
        None => {
            // 找不到「一行一条」的那条，但全文里其实有这个名字 —— 多半是
            // 格式和我们预期的不一样。这时候插进去会变成注册两遍，宁可不改。
            let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
            if compact.contains(&format!("\"name\":\"{BRIDGE_NAME}\"")) {
                return Err(Error::new(
                    "plugins.js 里已经有 wtool_bridge，但那条不是一个整行，格式不认识，没敢改",
                ));
            }

            let at = lines
                .iter()
                .position(|l| l.trim() == "];")
                .ok_or_else(|| Error::new("plugins.js 里找不到 `];`，格式不认识，没敢改"))?;
            lines.insert(at, String::new());
            changed = true;
            at
        }
    };

    // 桥前面那条记录必须以逗号收尾。
    //
    // RPG Maker 生成时数组的最后一条不带尾逗号，我们把桥插到它后面，
    // 就把它从「最后一条」变成了「中间那条」。不补逗号会写成
    // `{…}{…}` 这种非法形式，游戏启动直接报语法错误。
    //
    // 认「往上最近的一条以 `}` 结尾的行」，而不是「紧挨着的上一行」：
    // 桥是插进去的，我们和 `];` 之间可能本来就夹着空行。
    if let Some(i) = (0..at).rev().find(|&i| lines[i].trim_end().ends_with('}')) {
        lines[i] = lines[i].trim_end().to_string() + ",";
        changed = true;
    }

    // 桥自己这一行：内容是生成的那一条就写上去，顺便把「被手动关掉」的
    // 状态修正回来。
    let want = entry_line(BRIDGE_NAME);
    if lines[at] != want {
        lines[at] = want;
        changed = true;
    }

    // ---- 关掉探测插件 ----
    if let Some(i) = lines.iter().position(|l| is_entry_named(l, PROBE_NAME)) {
        if let Some(off) = disable_status(&lines[i]) {
            lines[i] = off;
            changed = true;
        }
    }

    if !changed {
        return Ok((src.to_string(), false));
    }

    Ok((join_lines(&lines, eol, src), true))
}

/// 改 `plugins.js` 的正文，把 `patch` 加进去的东西一一撤回来。
/// 返回（新正文，是否真的改动了什么）。
///
/// 和 `patch` 严格对称，但**刻意只按「桥那一行」定位**，不按 `];`：
/// 有些游戏在 `plugins.js` 末尾追加过脚本，`];` 可能不止一处，
/// 按它定位会认错地方。
fn unpatch(src: &str) -> (String, bool) {
    let eol = if src.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();

    let Some(first) = lines.iter().position(|l| is_entry_named(l, BRIDGE_NAME)) else {
        return (src.to_string(), false);
    };

    // 桥原本是不是 `$plugins` 里的最后一条？是的话，`patch` 把前一条从
    // 「最后一条」变成了「中间那条」并补了逗号，这里要对称地还回去。
    //
    // 尾逗号在 JS 里合法、游戏不会崩，纯粹是为了「删掉插件即完全恢复」
    // 这个承诺 —— 差一个字节都不算。
    let close = lines
        .iter()
        .position(|l| l.trim() == "];")
        .unwrap_or(lines.len());
    let was_last = first < close
        && !lines[first + 1..close]
            .iter()
            .any(|l| l.trim_start().starts_with('{'));

    if was_last {
        if let Some(i) = (0..first)
            .rev()
            .find(|&i| lines[i].trim_end().ends_with("},"))
        {
            let trimmed = lines[i].trim_end();
            lines[i] = trimmed[..trimmed.len() - 1].to_string();
        }
    }

    // 可能不止一条：用户手动复制过就会有两份，一起清掉
    // —— 留着会挂两次钩子，同一段对话上报两遍。
    for i in (0..lines.len()).rev() {
        if is_entry_named(&lines[i], BRIDGE_NAME) {
            lines.remove(i);
        }
    }

    (join_lines(&lines, eol, src), true)
}

/// 把行拼回正文。`lines()` 会把 CRLF 的 `\r` 吃掉，这里按原样补回去；
/// 结尾那个换行也要照原样还，才能做到逐字节相同。
fn join_lines(lines: &[String], eol: &str, original: &str) -> String {
    let mut out = lines.join(eol);
    if original.ends_with('\n') {
        out.push_str(eol);
    }
    out
}

/// 先写同目录的 `.tmp` 再 rename。
///
/// 直接 `fs::write` 是「先截断再写」，崩在中间（断电、被强杀）会留下半个
/// `plugins.js` —— 游戏加载不了插件，起不来。rename 在同一个分区上是原子的。
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("js.wtool.tmp");
    std::fs::write(&tmp, content)?;
    // Windows 上 rename 到已经存在的文件会失败，得先把目标删掉。这一步不是
    // 原子的，但窗口极小，而且此时 `.tmp` 已经完整落盘了。
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 这一行是不是 `$plugins` 里名为 `name` 的那一条。
///
/// RPG Maker 生成的是一个插件一行，所以按行认就够了，不必真去解析 JS。
fn is_entry_named(line: &str, name: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains(&format!("\"name\":\"{name}\""))
}

/// 一条 `$plugins` 记录，风格照着引擎自己生成的样子来。
fn entry_line(name: &str) -> String {
    let mut s = String::from("{\"name\":\"");
    s.push_str(name);
    s.push_str("\",\"status\":true,\"parameters\":{}}");
    s
}

/// 把这一行里的 `"status":true` 改成 `false`。认不出来就返回 `None`。
fn disable_status(line: &str) -> Option<String> {
    let at = line.find("\"status\"")?;
    let after = at + "\"status\"".len();
    // 紧跟其后的 `true` 才是状态值。中间只允许冒号和空白，
    // 否则万一 description 里有 "true" 会被误伤。
    let rel = line[after..].find("true")?;
    if !line[after..after + rel].chars().all(|c| c == ':' || c.is_whitespace()) {
        return None;
    }
    let t = after + rel;

    let mut out = String::with_capacity(line.len());
    out.push_str(&line[..t]);
    out.push_str("false");
    out.push_str(&line[t + 4..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试里装的是实时那份源码（`install_with` 的正文参数在这里补上）。
    fn install(root: &Path) -> Result<Vec<String>> {
        install_with(root, BRIDGE_JS)
    }

    /// 一次性的游戏目录。项目一贯不新增 crate（连 `link.rs` 都只用 std），
    /// 测试也不为这点事去引 `tempfile`。
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wtool_bridge_test_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("js").join("plugins")).unwrap();
        dir
    }

    /// 一份「正常」的 `plugins.js`：前面有别的插件，最后一条没有尾逗号
    /// （RPG Maker 就是这么生成的）。
    const LIST: &str = concat!(
        "// Generated by RPG Maker.\n",
        "// Do not edit this file directly.\n",
        "var $plugins =\n",
        "[\n",
        "{\"name\":\"wtool_reader\",\"status\":true,\"parameters\":{}},\n",
        "{\"name\":\"OtherPlugin\",\"status\":true,\"parameters\":{}}\n",
        "];\n",
    );

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// 装完再卸，必须和原来逐字节相同 —— 这是「删掉插件即完全恢复」的承诺。
    #[test]
    fn roundtrip_is_byte_exact() {
        let root = scratch("roundtrip");
        let list = root.join("js").join("plugins.js");
        std::fs::write(&list, LIST).unwrap();

        install(&root).unwrap();
        let installed = read(&list);
        assert!(installed.contains("wtool_bridge"));
        // 桥插在最后一条后面，前面那条得被补上逗号，否则是非法 JS。
        assert!(installed.contains("{\"name\":\"OtherPlugin\",\"status\":true,\"parameters\":{}},"));
        // 探测插件要被关掉，否则两个插件会挂两次钩子。
        assert!(installed.contains("\"name\":\"wtool_reader\",\"status\":false"));

        uninstall(&root).unwrap();
        assert_eq!(read(&list), LIST);
        assert!(!root
            .join("js")
            .join("plugins")
            .join("wtool_bridge.js")
            .exists());
    }

    /// CRLF 的游戏目录也要原样还回去。
    #[test]
    fn roundtrip_keeps_crlf() {
        let root = scratch("crlf");
        let list = root.join("js").join("plugins.js");
        let crlf = LIST.replace('\n', "\r\n");
        std::fs::write(&list, &crlf).unwrap();

        install(&root).unwrap();
        uninstall(&root).unwrap();
        assert_eq!(read(&list), crlf);
    }

    /// 没装过就卸：一个字都不该动，也不该报错。
    #[test]
    fn uninstall_without_install_changes_nothing() {
        let root = scratch("noop");
        let list = root.join("js").join("plugins.js");
        std::fs::write(&list, LIST).unwrap();

        let log = uninstall(&root).unwrap();
        assert_eq!(read(&list), LIST);
        assert!(log.iter().any(|l| l.contains("没有 wtool_bridge")));
    }

    /// 用户手动复制出两条桥，卸载要一起清掉 —— 留着会挂两次钩子。
    #[test]
    fn uninstall_removes_duplicates() {
        let root = scratch("dup");
        let list = root.join("js").join("plugins.js");
        std::fs::write(&list, LIST).unwrap();
        install(&root).unwrap();

        let dup = read(&list).replacen(
            "{\"name\":\"wtool_bridge\"",
            "{\"name\":\"wtool_bridge\",\"status\":true,\"parameters\":{}},\n{\"name\":\"wtool_bridge\"",
            1,
        );
        std::fs::write(&list, &dup).unwrap();

        uninstall(&root).unwrap();
        assert!(!read(&list).contains("wtool_bridge"));
    }

    /// 装完之后用户又自己加了插件 —— 这时**不能**拿备份整文件还原，
    /// 否则会把新插件一起抹掉。只撤我们加的那部分。
    #[test]
    fn uninstall_keeps_plugins_added_after_install() {
        let root = scratch("minimal");
        let list = root.join("js").join("plugins.js");
        std::fs::write(&list, LIST).unwrap();
        install(&root).unwrap();

        let later = read(&list).replace(
            "{\"name\":\"wtool_bridge\",\"status\":true,\"parameters\":{}}\n];",
            concat!(
                "{\"name\":\"wtool_bridge\",\"status\":true,\"parameters\":{}},\n",
                "{\"name\":\"AddedLater\",\"status\":true,\"parameters\":{}}\n];"
            ),
        );
        std::fs::write(&list, &later).unwrap();

        uninstall(&root).unwrap();
        let after = read(&list);
        assert!(after.contains("AddedLater"), "用户后加的插件不能被抹掉");
        assert!(!after.contains("wtool_bridge"));
        // 剩下的必须仍是合法 JS：后加的插件是最后一条，不带尾逗号。
        assert!(after.contains("{\"name\":\"AddedLater\",\"status\":true,\"parameters\":{}}\n];"));
    }

    /// 探测：没装是 `Ready`，装了是 `Installed`，被关掉是 `Stale`。
    #[test]
    fn probe_reports_each_step() {
        let root = scratch("probe");
        let list = root.join("js").join("plugins.js");
        std::fs::write(&list, LIST).unwrap();
        assert_eq!(JsPluginBridge.probe(&root).state, State::Ready);

        install(&root).unwrap();
        assert_eq!(JsPluginBridge.probe(&root).state, State::Installed);

        let off = read(&list).replace(
            "{\"name\":\"wtool_bridge\",\"status\":true",
            "{\"name\":\"wtool_bridge\",\"status\":false",
        );
        std::fs::write(&list, off).unwrap();
        assert_eq!(JsPluginBridge.probe(&root).state, State::Stale);
    }

    /// 目录填错是「探测结果」，不是崩溃。
    #[test]
    fn probe_on_wrong_dir_is_not_applicable() {
        let dir = std::env::temp_dir().join("wtool_bridge_test_no_such_game");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(JsPluginBridge.probe(&dir).state, State::NotApplicable);
    }
}