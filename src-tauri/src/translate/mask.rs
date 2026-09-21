//! 控制码遮蔽（mask）与还原。
//!
//! 游戏文本里的控制码（`\c[2]` 颜色、`\i[127]` 图标、`\I[188]` 图标、`\v[n]`
//! 变量、`\p[1]` 说话人…）是给游戏引擎看的指令，不是玩家看的字。直接整段丢给
//! LLM，它很可能替你「翻译」掉——把 `\c[2]` 挪走、或译成中文——结果游戏那一段
//! 就没颜色/没图标了。
//!
//! 所以发模型之前先把控制码摘出来、换成不易被改的占位符 `[[0]]` `[[1]]`…，
//! 模型只翻剩下的真话；收到译文再把占位符换回原始控制码。`[[N]]` 不像会出现在
//! 日语原文里，也不像会被模型写错，是当占位符的安全选择。
//!
//! 扫描控制码的逻辑和 `crate::corpus::strip_controls` 平行：那边是「把控制码
//! **删掉**看剩没剩值得翻的字」，这里是「把控制码**留着**分别编号」。两处各自
//! 独立实现，别强行合并——需求不同，硬揉在一起反而两个都别扭。
//!
//! 换行、制表符**不** mask：它们是结构（模型要照着保留行数），而且 JSON 里体现
//! 的是真实 `\n`/`\t`，模型照抄就行。

use crate::error::{Error, Result};

/// 把一条文本里的控制码替换成占位符，返回（mask 后的串，占位符对原控制码）。
///
/// 没有控制码的行原样返回、`originals` 为空——调用方可以靠它跳过无谓的 unmask。
pub fn mask(s: &str) -> (String, Vec<String>) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(s.len());
    let mut original: Vec<String> = Vec::new();
    let mut i = 0;

    while i < n {
        if bytes[i] == b'\\' {
            // 反斜杠后跟一串字母才是控制码（\c、\i、\v、\p…），可选带 [..]。
            let mut j = i + 1;
            while j < n && bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > i + 1 {
                let end = if j < n && bytes[j] == b'[' {
                    match s[j..].find(']') {
                        Some(off) => j + off + 1,
                        None => n,
                    }
                } else {
                    j
                };
                original.push(s[i..end].to_string());
                out.push_str(&format!("[[{}]]", original.len() - 1));
                i = end;
                continue;
            }
            // 孤零零一个反斜杠，不是控制码：照抄。
            out.push('\\');
            i += 1;
        } else {
            let c = s[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }

    (out, original)
}

/// 把译文里的占位符换回控制码。
///
/// 模型若把 `[[N]]` 弄丢了、改样了、或数字对不上占位符个数，**当场报错**，
/// 不静默吞——控制码错一个就是一段画面字变色/丢图标，这种事宁可不写也不能
/// 写错。占满模式：每 `[[N]]` 必须落到 `originals[N]`，多了少了都是错。
pub fn unmask(s: &str, originals: &[String]) -> Result<String> {
    if originals.is_empty() {
        // 没 mask 过：原文里本就没有控制码，译文里的 `[[…]]` 是字号面文本
        // （有的游戏文本写作 `[[0]]`、以及 `%1` 这类记号，模型照抄过来了），
        // 不是我们埋的占位符 —— 原样放行，别误当成坏占位符报错。
        return Ok(s.to_string());
    }

    let mut out = String::with_capacity(s.len());
    // 记下哪些控制码已经被还原。模型若偷偷删了某个占位符，
    // 结束后一数就知道「原文有 2 个、你只还了 1 个」。
    let mut seen = vec![false; originals.len()];
    let mut used = 0usize;
    let mut it = s.chars().peekable();

    // 每遇到 [[ 就试着读一个数字 + ]]，再按 index 换回。
    while let Some(c) = it.next() {
        if c != '[' {
            out.push(c);
            continue;
        }
        // c == '['：必须是 [[，否则是模型写的普通方括号，照抄。
        if it.peek() != Some(&'[') {
            out.push(c);
            continue;
        }
        it.next(); // 吃掉第二个 [
        let mut idx = String::new();
        let mut closed = false;
        while let Some(&d) = it.peek() {
            if d == ']' {
                it.next();
                closed = true;
                if it.peek() == Some(&']') {
                    it.next();
                } else {
                    return Err(Error::new(format!("译文里的占位符少了一个 `]`：{s}")));
                }
                break;
            } else if d.is_ascii_digit() {
                idx.push(d);
                it.next();
            } else {
                it.next(); // 别卡死：跳过这个字符再看下一个
            }
        }
        if !closed {
            return Err(Error::new(format!("译文里的占位符没有闭合：{s}")));
        }
        let idx = idx
            .parse::<usize>()
            .map_err(|_| Error::new(format!("译文里的占位符不合法：{s}")))?;
        let slot = seen
            .get_mut(idx)
            .ok_or_else(|| Error::new(format!("译文引用了一个不存在的占位符 [[{idx}]]：{s}")))?;
        // 同一个控制码还原两遍 = 模型把 [[N]] 抄错了，别让它悄悄通过。
        if *slot {
            return Err(Error::new(format!("译文把占位符 [[{idx}]] 用了两遍：{s}")));
        }
        *slot = true;
        used += 1;
        out.push_str(&originals[idx]);
    }

    // 占位符个 VS 原文控制码个数对不上，说明模型删了/并了占位符 —— 报错不硬猜。
    if used != originals.len() {
        return Err(Error::new(format!(
            "译文丢了 {} 个占位符：原文有 {} 个控制码，译文只还原了 {} 个（{s}）",
            originals.len() - used,
            originals.len(),
            used
        )));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_replaces_control_codes_in_order() {
        let (m, o) = mask("\\c[2]赤い文字\\c[0]");
        assert_eq!(m, "[[0]]赤い文字[[1]]");
        assert_eq!(o, ["\\c[2]", "\\c[0]"]);
    }

    #[test]
    fn mask_keeps_icon_and_variable_codes() {
        let (m, o) = mask("\\I[188]手帳 \\v[439]の状態");
        assert_eq!(m, "[[0]]手帳 [[1]]の状態");
        assert_eq!(o, ["\\I[188]", "\\v[439]"]);
    }

    /// 纯文本行 mask 幂等，originals 为空。
    #[test]
    fn mask_is_identity_on_plain_text() {
        let (m, o) = mask("こんにちは、世界！");
        assert_eq!(m, "こんにちは、世界！");
        assert!(o.is_empty());
    }

    /// 换行和制表符不 mask。
    #[test]
    fn mask_keeps_newline_and_tab() {
        let (m, o) = mask("一行目\n二行目\tタブ");
        assert_eq!(m, "一行目\n二行目\tタブ");
        assert!(o.is_empty());
    }

    /// 孤零零的反斜杠不是控制码，照抄。
    #[test]
    fn mask_keeps_lone_backslash() {
        let (m, _) = mask("パスは C:\\ です");
        assert_eq!(m, "パスは C:\\ です");
    }

    /// mask → unmask 字节一致还原。
    #[test]
    fn roundtrip_restores_original_control_codes() {
        let s = "\\c[2]赤い文字\\c[0]\\n改行";
        let (m, o) = mask(s);
        let back = unmask(&m, &o).unwrap();
        assert_eq!(back, s);
    }

    /// 模型把占位符弄丢 → 报错不硬猜。
    #[test]
    fn unmask_errors_when_placeholder_dropped() {
        let (_m, o) = mask("\\c[2]赤い文字\\c[0]");
        let r = unmask("赤い文字", &o);
        assert!(r.is_err());
    }

    /// 原文没有控制码，但自带字面 `[[0]]`、`%1` 这类记号，模型照抄 —— 不报错。
    #[test]
    fn unmask_keeps_literal_brackets_when_no_control_codes() {
        let r = unmask("失误！[[0]]%1没有受到伤害！", &[]).unwrap();
        assert_eq!(r, "失误！[[0]]%1没有受到伤害！");
    }
}