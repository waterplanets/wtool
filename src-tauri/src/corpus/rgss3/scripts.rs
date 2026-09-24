//! `Data/Scripts.rvdata2` 抽取：先解 zlib，再从 Ruby 源码里抠字符串字面量。
//!
//! 为什么非得单独扫这个文件：`Scripts.rvdata2` 里装的是**整个游戏的 Ruby 源码**
//! （实测 11,223,736 字节 / 270 段脚本），而 WF-RGSS 这套脚本的文案是**硬编码在
//! 源码字面量里**的 —— 图鉴说明、帮助文本、术语表、事件文字都存在
//! `:word_1 => ["【アルルーナ】\nどうして植物妖魔に…"]`、`:help => "多段スキル…"`
//! 这种形态里，不在任何一张数据表里。不扫它，这批文案就整批漏掉。
//!
//! 两条坑：
//!
//! 1. **源码是压缩的**。每段脚本在 Marshal 里是一条 binary 字符串，内容是 zlib
//!    流。所以不能走 [`rvdata2::walk`]（它按「非 UTF-8 → 丢」处理，整段会消失），
//!    要用 [`rvdata2::walk_blobs`] 拿原始字节再 inflate。
//! 2. **不能整段当词条**。源码里有注释、变量名、Ruby 语法，整段收进来是垃圾；
//!    真正要给玩家看的字只在字符串字面量里。所以照
//!    [`crate::corpus::plugins::extract_string_literals`] 的思路写一个 Ruby 版
//!    手写扫描器：只认 `'`/`"`，跳过 `#` 行注释与 `=begin`/`=end` 块注释，
//!    引号没闭合就整条丢（那种「串」一吞就是大半段代码）。
//!
//! 收进来的还是**原样**（含 `\n`、含尾标点），分粒度、去噪、够不够格都在
//! `scan.rs` 里统一做 —— 这里是纯格式层，和 [`rvdata2`] 一个定位。

use std::io::Read;

use flate2::read::ZlibDecoder;

use crate::corpus::rgss3::rvdata2;
use crate::error::Result;

/// 从 `Scripts.rvdata2` 的原始字节里抽出所有字符串字面量。
pub fn extract(raw: &[u8]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for blob in rvdata2::walk_blobs(raw)? {
        // 解不出来 / 不是 UTF-8 的 blob 直接跳过：270 段里绝大多数都是源码，
        // 但不是每一条 binary 串都是 zlib（引擎自己也可能塞别的）。
        let Some(src) = inflate(&blob) else {
            continue;
        };
        literals(&src, &mut out);
    }
    Ok(out)
}

/// 一条 zlib 流 → 源码文本。
fn inflate(blob: &[u8]) -> Option<String> {
    let mut out = Vec::new();
    ZlibDecoder::new(blob).read_to_end(&mut out).ok()?;
    String::from_utf8(out).ok()
}

/// 从一段 Ruby 源码里抽出字符串字面量的**内容**（去引号、解转义）。
///
/// 只认 `'` 和 `"` 两种引号（Ruby 的 `%w()`/`%q()`/heredoc 在目标游戏里没用，
/// 见测试 `ignores_non_quote_constructs`）。两类注释都要跳：`#` 到行尾，
/// 以及必须顶格写的 `=begin`/`=end`；`__END__` 之后是数据段，直接收工。
///
/// 解转义时**单双引号分开对待**：Ruby 里 `'\n'` 是两个字符（反斜杠 + n），
/// 只有 `"…"` 才把 `\n` 解成换行 —— 混着解会把单引号里的路径、正则写成怪串。
fn literals(src: &str, out: &mut Vec<String>) {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut line_start = true;
    while i < n {
        // 顶格的 `=begin`/`=end`/`__END__` 只在行首生效，别处出现是普通标识符。
        if line_start {
            if b[i..].starts_with(b"=begin") {
                i += 6;
                // 跳到顶格的 `=end`（或者文件尾）。
                while i < n && !(line_start && b[i..].starts_with(b"=end")) {
                    line_start = b[i] == b'\n';
                    i += 1;
                }
                continue;
            }
            if b[i..].starts_with(b"__END__") {
                return;
            }
        }
        match b[i] {
            b'\n' => {
                line_start = true;
                i += 1;
            }
            b'#' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            quote @ (b'"' | b'\'') => {
                i += 1;
                let mut buf: Vec<u8> = Vec::new();
                let mut closed = false;
                while i < n {
                    let c = b[i];
                    if c == b'\\' && i + 1 < n {
                        // 双引号串才解转义；单引号串里只有 `\\` 和 `\'` 是转义。
                        let e = b[i + 1];
                        if quote == b'"' {
                            match e {
                                b'n' => buf.push(b'\n'),
                                b't' => buf.push(b'\t'),
                                b'r' => buf.push(b'\r'),
                                b'\\' => buf.push(b'\\'),
                                b'"' => buf.push(b'"'),
                                other => {
                                    buf.push(b'\\');
                                    buf.push(other);
                                }
                            }
                        } else if e == b'\\' || e == b'\'' {
                            buf.push(e);
                        } else {
                            buf.push(b'\\');
                            buf.push(e);
                        }
                        i += 2;
                    } else if c == quote {
                        i += 1;
                        closed = true;
                        break;
                    } else {
                        buf.push(c);
                        i += 1;
                    }
                }
                // 引号没闭合 = 这根本不是字符串（别处的撇号、被截断的代码）。
                // 这种「串」一吞就是大半段源码，丢掉比收进来安全。
                if !closed {
                    line_start = false;
                    continue;
                }
                // 到这儿才解码：中途 `byte as char` 会把一个 UTF-8 多字节序列
                // 拆成 Latin-1 乱码，含日文的判断就全假了（同 plugins.rs 的坑）。
                out.push(String::from_utf8_lossy(&buf).into_owned());
                line_start = false;
            }
            _ => {
                line_start = false;
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        literals(src, &mut out);
        out
    }

    /// 双引号串的 `\n` 解成真换行（WF-RGSS 的文案正是靠它拼多行）。
    #[test]
    fn unescapes_double_quoted_newlines() {
        let got = scan(r#"x = "一行目\n二行目""#);
        assert_eq!(got, ["一行目\n二行目"]);
    }

    /// 单引号串只认 `\\` 和 `\'`，`\n` 保持两个字面字符 —— 这是 Ruby 的语义。
    #[test]
    fn keeps_single_quoted_backslash() {
        let got = scan(r"x = '一行目\n二行目'");
        assert_eq!(got, [r"一行目\n二行目"]);
    }

    /// `#` 行注释里的日文不算文案；注释里的撇号也不能开一个假字符串。
    #[test]
    fn skips_line_comments() {
        let got = scan("# ここは説明です\ny = \"本文\" # これもコメント\n");
        assert_eq!(got, ["本文"]);
    }

    /// `=begin`/`=end` 块注释整段跳过（顶格才认）。
    #[test]
    fn skips_block_comments() {
        let got = scan("=begin\nこれはコメント\n'偽の引用符'\n=end\nz = \"本体\"\n");
        assert_eq!(got, ["本体"]);
    }

    /// 没闭合的引号不能把后面一大段源码吞成一个「字符串」。
    #[test]
    fn drops_unclosed_quote() {
        let got = scan("it's fine\nvar x = 1\n");
        assert!(got.is_empty(), "{got:?}");
    }

    /// `__END__` 之后是数据段，不再当代码扫。
    #[test]
    fn stops_at_end_marker() {
        let got = scan("a = \"本文\"\n__END__\n\"データ\"\n");
        assert_eq!(got, ["本文"]);
    }

    /// 目标游戏里没有的写法（`%q()`、heredoc）不假装支持：里面的字收不到，
    /// 但**不能**因此把后面的正常代码吞掉。真出现了再来加规则。
    #[test]
    fn ignores_non_quote_constructs() {
        let got = scan("w = %w(あ い)\nsql = <<~SQL\n  ほげ\nSQL\nafter = \"後ろ\"\n");
        assert!(got.contains(&"後ろ".to_string()), "{got:?}");
        assert!(!got.iter().any(|s| s.contains("SQL")), "{got:?}");
    }
}