//! Ruby Marshal（`*.rvdata2`）遍历器：走一遍序列化对象树，挖出里面的字符串。
//!
//! 为什么不用「按字节找 `"` 再猜长度」那种启发式（Python 探针里那版）：
//!
//! - 它只认单字节长度（0..122），**超过 122 字节的字符串整个跳过** ——
//!   日文 UTF-8 一个字 3 字节，122 字节才 40 来字，大段对话常常超；
//! - 它没有结构信息，判不出「这条是整段还是单行」，也没法给词条起名字；
//! - 一旦撞上二进制数据就会产出乱码语料。
//!
//! 所以这里老老实实按 Marshal 的标签递归下降。要认的标签：
//!
//! ```text
//!   0       nil           T/F     true/false
//!   i       定点数        l       bignum（跳过）
//!   f       浮点（跳过）  :/;     符号 / 符号引用
//!   "       字符串        I       带 ivar 的值（Ruby 1.9 的字符串编码标记挂这）
//!   [       数组          {/}     哈希 / 带默认值的哈希
//!   o       对象          S       结构体
//!   u/U/d   自定义序列化（跳过载荷）  /       正则（跳过）
//!   c/m     类 / 模块     @       对象引用
//!   C       容器子类      e       扩展模块
//! ```
//!
//! `u` 必须**按长度整段跳过**：RGSS 的 `Table`/`Color`/`Tone`/`Rect`
//! 走的就是 `_dump` 自定义序列化，载荷是纯二进制，当成字符串读会吐出乱码。

use crate::error::{Error, Result};

/// 挖出来的一条文本：值 + 它在对象树里的位置。
///
/// 路径形如 `Data/Map001.rvdata2:@events[3].@pages[0].@list[12].@parameters[0]`。
/// 粒度分类就按它推（见 `scan.rs`）—— 同一条字符串，出现在 `@name` 里是
/// 「整串画」的片段，出现在事件指令参数里是「一行一条」，光看值本身分不出来。
#[derive(Debug, Clone)]
pub struct Text {
    pub path: String,
    pub value: String,
}

/// 走一遍 `raw`，返回里面所有能解成 UTF-8 的字符串。`root` 是路径的起点（文件名）。
pub fn walk(raw: &[u8], root: &str) -> Result<Vec<Text>> {
    let mut r = Reader::open(raw, None)?;
    let mut out = Vec::new();
    r.value(root, &mut out)?;
    Ok(out)
}

/// 走一遍 `raw`，返回里面**所有**字符串的原始字节，不做 UTF-8 过滤。
///
/// 为什么另开一条通道（而不是把 [`walk`] 放宽）：`Data/Scripts.rvdata2` 里
/// 每段脚本都是一条 zlib 压缩过的二进制串，按 [`walk`] 的口径会被
/// [`Reader::string`] 当「非 UTF-8 → 丢」直接扔掉 —— 实测那份文件
/// 11,223,736 字节里 270 段源码就是这么消失的。这里只给需要解压缩的调用方用，
/// 普通数据表照旧走 [`walk`]，乱码出不来。
pub fn walk_blobs(raw: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut r = Reader::open(raw, Some(Vec::new()))?;
    let mut out = Vec::new();
    r.value("", &mut out)?;
    Ok(r.blobs.unwrap_or_default())
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Marshal 的符号表：每个 `:` 按在流里出现的顺序追加，`;` 按下标引用。
    ///
    /// **必须照着维护**：ivar 名字绝大多数是 `;` 引用的（同一个名字只有第一次
    /// 出现时才写 `:`）。不查表的话，除了第一个对象，所有路径都会长成
    /// `.sym114` 这种样子 —— 而 `scan.rs` 的粒度分类正是按路径判的，
    /// `@name` 认不出来，菜单名那类「整串画」的文本就全被判成单行。
    symbols: Vec<String>,
    /// `Some` 时把解不成 UTF-8 的字符串正文也收下来（见 [`walk_blobs`]）；
    /// `None` 时保持原行为，二进制内容照旧丢弃。
    blobs: Option<Vec<Vec<u8>>>,
}

impl<'a> Reader<'a> {
    /// 校验 Marshal 版本头，从偏移 2 开始读。
    ///
    /// 头 2 字节是版本（`\x04\x08` = 4.8），不是数据的一部分。不跳过的话第一个
    /// 字节会被当成标签，读到 0x04 直接报「不认识的标签」。
    fn open(raw: &'a [u8], blobs: Option<Vec<Vec<u8>>>) -> Result<Self> {
        let (&major, &minor) = match raw {
            [major, minor, ..] => (major, minor),
            _ => return Err(Error::new("转储太短，不像 Marshal 数据")),
        };
        if major != 4 {
            return Err(Error::new(format!(
                "不认的 Marshal 版本 {major}.{minor}（只见过 4.8）"
            )));
        }
        Ok(Self {
            buf: raw,
            pos: 2,
            symbols: Vec::new(),
            blobs,
        })
    }

    fn byte(&mut self) -> Result<u8> {
        let b = *self.buf.get(self.pos).ok_or_else(|| self.eof())?;
        self.pos += 1;
        Ok(b)
    }

    /// 退回一个字节。用于「先看一眼标签、发现不对再当值读」的地方。
    fn unread(&mut self) {
        self.pos -= 1;
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let buf = self.buf;
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= buf.len())
            .ok_or_else(|| self.eof())?;
        let s = &buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn eof(&self) -> Error {
        Error::new(format!("Marshal 数据在偏移 {} 处断了", self.pos))
    }

    /// Ruby 的变长整数，Marshal 规格里管这个叫 long。
    ///
    /// 高字节那一档（`0x01..0x04`）是**「后面跟几字节小端」的正数**，不是负数：
    /// `0x02 0x2C 0x01` 是 300，不是 -3。对应的负数档是 `0xFC..0xFF`，
    /// 值 = 小端读数 - 2^(8k)。
    /// 剩下的是单字节档，符号扩展后再减/加 5：`0x05..0x7F` → 0..122、
    /// `0x80..0xFB` → -123..0。
    fn long(&mut self) -> Result<i64> {
        let b = self.byte()?;
        Ok(match b {
            0x00 => 0,
            0x01 => self.raw(1)?,
            0xFF => self.raw(1)? - 0x100,
            0x02 => self.raw(2)?,
            0xFE => self.raw(2)? - 0x1_0000,
            0x03 => self.raw(3)?,
            0xFD => self.raw(3)? - 0x100_0000,
            0x04 => self.raw(4)?,
            0xFC => self.raw(4)? - 0x1_0000_0000,
            _ => {
                let s = b as i8 as i64;
                if s > 0 {
                    s - 5
                } else {
                    s + 5
                }
            }
        })
    }

    /// 读 `n` 字节小端无符号数，给 [`Self::long`] 的多字节档用。
    fn raw(&mut self, n: usize) -> Result<i64> {
        let mut v: i64 = 0;
        for i in 0..n {
            v |= (self.byte()? as i64) << (8 * i);
        }
        Ok(v)
    }

    /// 长度/计数用的正整数版本。负数或离谱的大数一律报错，
    /// 免得后面拿着它去切字节。
    fn count(&mut self) -> Result<usize> {
        let n = self.long()?;
        if n < 0 || n > self.buf.len() as i64 {
            return Err(Error::new(format!(
                "Marshal 长度 {n} 不合理（偏移 {}）",
                self.pos
            )));
        }
        Ok(n as usize)
    }

    /// 符号的正文（标签 `:` 已经吃掉了）。
    fn symbol_body(&mut self) -> Result<String> {
        let n = self.count()?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    /// `:` 定义的符号：正文读出来，顺带进符号表（下标就是它的编号）。
    fn define_symbol(&mut self) -> Result<String> {
        let s = self.symbol_body()?;
        self.symbols.push(s.clone());
        Ok(s)
    }

    /// `;` 引用的符号：拿下标去查表。越界就退回 `symN` —— 数据坏了也得走完，
    /// 不能为了一条路径半路放弃整个文件。
    fn symbol_ref(&mut self) -> Result<String> {
        let idx = self.long()?;
        Ok(match usize::try_from(idx).ok().and_then(|i| self.symbols.get(i)) {
            Some(s) => s.clone(),
            None => format!("sym{idx}"),
        })
    }

    fn value(&mut self, path: &str, out: &mut Vec<Text>) -> Result<()> {
        let tag = self.byte()?;
        match tag {
            b'0' | b'T' | b'F' => {}
            b'i' => {
                self.long()?;
            }
            // bignum：符号位 1 字节 + 长度 + 载荷。
            //
            // 长度单位是**16 位字**，不是字节 —— 载荷长度 = `len * 2`。
            // 按字节读会少读一半：`DataEx.rvdata2` 里第一个 bignum 出现在
            // 33MB 处（敌人血量那类天文数字），少读一半后整棵树从那里歪掉，
            // 报的是后面某处「长度 -77 不合理」，离真正的原因十万八千里。
            b'l' => {
                self.byte()?;
                let n = self.count()?;
                self.take(n * 2)?;
            }
            // 浮点：长度 + 字节序列（短的 3 字节，长的是一串文本）。
            // 地图里的 `@parallax_sx` 这类就是浮点，长度读错整棵树就跟着歪。
            b'f' => {
                let n = self.count()?;
                self.take(n)?;
            }
            b'g' => {
                self.take(8)?;
            }
            b':' => {
                self.define_symbol()?;
            }
            b';' => {
                self.symbol_ref()?;
            }
            b'"' => self.string(path, out)?,
            // 带 ivar 的值：先是被包住的値，再是一张 ivar 表。
            // Ruby 1.9 的字符串就长这样（`I` + `"` + 内容 + 1 + `:E` + `T`）。
            b'I' => {
                self.value(path, out)?;
                self.members(path, out)?;
            }
            b'[' => {
                let n = self.count()?;
                for i in 0..n {
                    self.value(&format!("{path}[{i}]"), out)?;
                }
            }
            b'{' => {
                let n = self.count()?;
                for _ in 0..n {
                    let key = self.hash_key()?;
                    self.value(&format!("{path}[{key}]"), out)?;
                }
            }
            // 带默认值的哈希：多一个默认值挂在最后。
            b'}' => {
                let n = self.count()?;
                for _ in 0..n {
                    let key = self.hash_key()?;
                    self.value(&format!("{path}[{key}]"), out)?;
                }
                self.value(&format!("{path}.default"), out)?;
            }
            b'o' | b'S' => {
                self.symbol()?; // 类名 / 结构体名，路径里用不上
                self.members(path, out)?;
            }
            // 自定义序列化（Table/Color/Tone/Rect…）：类名 + 长度 + 二进制载荷。
            b'u' | b'd' => {
                self.symbol()?;
                let n = self.count()?;
                self.take(n)?;
            }
            // `U`（`marshal_dump`）不是字节序列：载荷本身还是一个 Marshal 值。
            b'U' => {
                self.symbol()?;
                self.value(path, out)?;
            }
            // 容器子类（String/Array/Hash 的 subclass）：类名 + 父类的值。
            b'C' => {
                self.symbol()?;
                self.value(path, out)?;
            }
            // 扩展模块（`extend M`）：成员表里混进来的一条，载荷是模块名。
            b'e' => {
                self.symbol()?;
            }
            b'/' => {
                let n = self.count()?;
                self.take(n)?;
                self.take(1)?; // 选项字节
            }
            b'c' | b'm' => {
                let n = self.count()?;
                self.take(n)?;
            }
            // 对象引用：指回前面出现过的对象，这里不需要它指到哪儿。
            b'@' => {
                self.long()?;
            }
            other => {
                return Err(Error::new(format!(
                    "不认识的 Marshal 标签 {:?}（偏移 {}）",
                    other as char,
                    self.pos - 1
                )))
            }
        }
        Ok(())
    }

    /// 符号：`:` 是正文（进表），`;` 是引用前面的符号（查表）。
    fn symbol(&mut self) -> Result<String> {
        match self.byte()? {
            b':' => self.define_symbol(),
            b';' => self.symbol_ref(),
            other => Err(Error::new(format!(
                "这里该是符号，读到的是 {:?}（偏移 {}）",
                other as char,
                self.pos - 1
            ))),
        }
    }

    /// `o`/`S`/`I` 三种都带一张「名字 → 值」的表，读法一样。
    ///
    /// 名字位置不硬认标签：正常的 ivar 是 `:`（符号）或 `;`（符号引用），
    /// 但扩展模块（`e`）这类条目整个只有标签 + 载荷、既没有名字也没有另外的值。
    /// 所以不是 `:`/`;` 就把那个字节退回去，按「值」再读一遍。
    fn members(&mut self, path: &str, out: &mut Vec<Text>) -> Result<()> {
        let n = self.count()?;
        for _ in 0..n {
            match self.byte()? {
                b':' => {
                    let name = self.define_symbol()?;
                    self.value(&format!("{path}.{name}"), out)?;
                }
                b';' => {
                    let name = self.symbol_ref()?;
                    self.value(&format!("{path}.{name}"), out)?;
                }
                _ => {
                    self.unread();
                    self.value(path, out)?;
                }
            }
        }
        Ok(())
    }

    /// 哈希的键。键只是为了给路径起个名，**不当语料收** —— RGSS 数据里的
    /// 键基本都是 id、开关号这类东西，不是给玩家看的字。
    fn hash_key(&mut self) -> Result<String> {
        let tag = self.byte()?;
        Ok(match tag {
            b'i' => self.long()?.to_string(),
            b'"' => self
                .string_body()?
                .unwrap_or_else(|| "?".to_string()),
            b':' => self.define_symbol()?,
            b';' => self.symbol_ref()?,
            b'0' => "nil".to_string(),
            b'T' => "true".to_string(),
            b'F' => "false".to_string(),
            _ => {
                // 罕见的键形状：整个当值跳过，路径上记个问号。
                self.unread();
                let mut scratch = Vec::new();
                self.value("", &mut scratch)?;
                "?".to_string()
            }
        })
    }

    fn string(&mut self, path: &str, out: &mut Vec<Text>) -> Result<()> {
        let n = self.count()?;
        let bytes = self.take(n)?;
        match std::str::from_utf8(bytes) {
            Ok(s) => out.push(Text {
                path: path.to_string(),
                value: s.to_string(),
            }),
            // 解不成 UTF-8：这个引擎里的日文文本一定是 UTF-8，所以这是二进制内容。
            // 平时丢掉（当文本收进去只会往语料里灌乱码）；`walk_blobs` 那条通道
            // 要求留着，由调用方自己解压/过滤。
            Err(_) => {
                if let Some(blobs) = self.blobs.as_mut() {
                    blobs.push(bytes.to_vec());
                }
            }
        }
        Ok(())
    }

    /// 读一个字符串的正文，只要 UTF-8 的那种。哈希键用（键本来就该是短的符号/文本）。
    fn string_body(&mut self) -> Result<Option<String>> {
        let n = self.count()?;
        let bytes = self.take(n)?;
        Ok(std::str::from_utf8(bytes).ok().map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把长度编码成 Marshal 的变长整数（只编正数），照 CRuby 挑最短形式：
    /// 0..122 一个字节 `n+5`；123..255 是 `0x01` + 一字节；再大是「字节数」+ 小端。
    fn long(n: usize) -> Vec<u8> {
        if n <= 122 {
            return vec![(n + 5) as u8];
        }
        if n <= 255 {
            return vec![0x01, n as u8];
        }
        let mut bytes = Vec::new();
        let mut x = n;
        while x > 0 {
            bytes.push((x & 0xFF) as u8);
            x >>= 8;
        }
        let mut out = vec![bytes.len() as u8];
        out.extend(bytes);
        out
    }

    /// `:名字` 符号。
    fn sym(name: &str) -> Vec<u8> {
        let mut v = vec![b':'];
        v.extend(long(name.len()));
        v.extend_from_slice(name.as_bytes());
        v
    }

    /// Ruby 1.9 的字符串：`I` + `"` + 内容 + 1 个 ivar（`:E` 编码标记）。
    fn str_node(s: &str) -> Vec<u8> {
        let mut v = vec![b'I', b'"'];
        v.extend(long(s.len()));
        v.extend_from_slice(s.as_bytes());
        v.extend(long(1));
        v.extend(sym("E"));
        v.push(b'T');
        v
    }

    /// 手工拼一小段真 Marshal：{ :name => "勇者", :n => 3, :tbl => Table 二进制 }。
    fn marshal_sample() -> Vec<u8> {
        let mut v = vec![0x04, 0x08, b'{'];
        v.extend(long(3)); // 3 条
        v.extend(sym("name"));
        v.extend(str_node("勇者"));
        v.extend(sym("n"));
        v.push(b'i');
        v.extend(long(3));
        v.extend(sym("tbl"));
        v.push(b'u');
        v.extend(sym("Table"));
        v.extend(long(5)); // 载荷 5 字节，内容是二进制
        v.extend_from_slice(&[0x00, 0x01, 0x02, 0xFE, 0xFF]);
        v
    }

    /// 字符串挖出来、路径带上哈希键、二进制载荷不产出乱码。
    #[test]
    fn walks_strings_and_skips_binary() {
        let raw = marshal_sample();
        let texts = walk(&raw, "Data/Test.rvdata2").unwrap();

        assert_eq!(texts.len(), 1, "只该有一条文本：{texts:?}");
        assert_eq!(texts[0].value, "勇者");
        assert_eq!(texts[0].path, "Data/Test.rvdata2[:name]");
        // Table 的二进制载荷没被当成字符串（没产出替换字符）
        assert!(!texts.iter().any(|t| t.value.contains('\u{FFFD}')));
    }

    /// 超长字符串（>122 字节）也要能读出来 —— 这正是启发式那版漏掉的部分。
    #[test]
    fn reads_long_strings() {
        let long_text = "あ".repeat(100); // 300 字节
        let mut v = vec![0x04, 0x08, b'"'];
        v.extend(long(long_text.len()));
        v.extend_from_slice(long_text.as_bytes());

        let texts = walk(&v, "x").unwrap();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].value, long_text);
    }

    /// 变长整数按 CRuby 的实际字节解。这几组都是 `Marshal.dump` 的真输出：
    /// `\xFA` = -1、`\x01\x7B` = 123、`\x02\x00\x01` = 256。
    ///
    /// 这里曾经把 `0x01..0x04` 当成 -4..-1，于是 `\x02\x2C\x01`（300）被读成
    /// -3 报错 —— 索引超过 122 条的表全解析不了。
    #[test]
    fn reads_longs_like_cruby() {
        for (bytes, want) in [
            (vec![0x00], 0i64),
            (vec![0x06], 1),
            (vec![0x7F], 122),
            (vec![0xFA], -1),
            (vec![0x80], -123),
            (vec![0x01, 0x7B], 123),
            (vec![0x01, 0xFF], 255),
            (vec![0xFF, 0x7C], -124),
            (vec![0xFF, 0x00], -256),
            (vec![0x02, 0x00, 0x01], 256),
            (vec![0x02, 0x2C, 0x01], 300),
            (vec![0xFE, 0xFF, 0xFE], -257),
            (vec![0x03, 0x00, 0x00, 0x01], 65536),
        ] {
            let mut r = Reader {
                buf: &bytes[..],
                pos: 0,
                symbols: Vec::new(),
                blobs: None,
            };
            assert_eq!(r.long().unwrap(), want, "{bytes:02X?}");
            assert_eq!(r.pos, bytes.len(), "{bytes:02X?} 没读完");
        }
    }

    /// 对象（`o`）里的 ivar 名字要进路径，值要收出来。
    #[test]
    fn walks_object_ivars() {
        let mut v = vec![0x04, 0x08, b'o'];
        v.extend(sym("Game_Actor"));
        v.extend(long(2));
        v.extend(sym("@name"));
        v.extend(str_node("アリス"));
        v.extend(sym("@level"));
        v.push(b'i');
        v.extend(long(7));

        let texts = walk(&v, "Data/Actors.rvdata2").unwrap();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert_eq!(texts[0].value, "アリス");
        assert_eq!(texts[0].path, "Data/Actors.rvdata2.@name");
    }

    /// ivar 名字用 `;` 引用时也要解析成**真名字**，不能写成 `sym0`。
    ///
    /// 这是实际数据里的常态：同一张表里，只有第一个对象的 ivar 名写 `:`，
    /// 后面的全写 `;`。以前不维护符号表，路径就成了 `.sym1` —— 于是
    /// `scan.rs` 里按 `@name` 判粒度的规则形同虚设（整份语料只有 30 条片段）。
    #[test]
    fn resolves_symbol_references_in_paths() {
        // 一个数组两个对象：[{@name => "アリス"}, {@name => "ボブ"}]。
        // 第二个对象的名字用 `;0` 引回前面的 `:@name`。
        let mut v = vec![0x04, 0x08, b'['];
        v.extend(long(2));
        for name in ["アリス", "ボブ"] {
            v.push(b'o');
            v.extend(sym("Game_Actor"));
            v.extend(long(1));
            // 第一个对象写 `:@name`（进表，成为 #1；#0 是 Game_Actor），
            // 第二个写 `;1` 引用它。
            if name == "アリス" {
                v.extend(sym("@name"));
            } else {
                v.push(b';');
                v.extend(long(1));
            }
            v.extend(str_node(name));
        }

        let texts = walk(&v, "Data/Actors.rvdata2").unwrap();
        let paths: Vec<&str> = texts.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(
            paths,
            ["Data/Actors.rvdata2[0].@name", "Data/Actors.rvdata2[1].@name"],
            "{texts:?}"
        );
    }

    /// bignum（`l`）的长度单位是「16 位字」：载荷 = `len * 2` 字节。
    ///
    /// 这里曾经按字节读。少读一半不会当场报错，只会让游标停在载荷中间，
    /// 从那个 bignum 起整棵树歪掉 —— 实际表现是几十 MB 之后某处冒出
    /// 「长度 -77 不合理」，完全指不回原因。
    #[test]
    fn reads_bignum_payload_in_words() {
        // 数组两个元素：一个 len=7 的 bignum（载荷 14 字节），一个字符串。
        // 载荷若只读 7 字节，剩下 7 个 0xFF 会被当成下一个元素的标签而报错。
        let mut v = vec![0x04, 0x08, b'['];
        v.extend(long(2));
        v.push(b'l');
        v.push(b'+');
        v.extend(long(7));
        v.extend_from_slice(&[0xFF; 14]);
        v.extend(str_node("勇者"));

        let texts = walk(&v, "x").unwrap();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert_eq!(texts[0].value, "勇者");
    }

    /// 不认识的标签要报错，不能猜着往下读（一猜就错位，后面全是垃圾）。
    #[test]
    fn rejects_unknown_tag() {
        let raw = vec![0x04, 0x08, b'Z'];
        let msg = walk(&raw, "x").unwrap_err().message;
        assert!(msg.contains("不认识的 Marshal 标签"), "{msg}");
    }

    /// 二进制字符串只有 `walk_blobs` 收得到，`walk` 照旧不吐（不乱码进语料）。
    /// `Data/Scripts.rvdata2` 里每段 zlib 压缩的源码就走这条通道。
    #[test]
    fn walk_blobs_keeps_binary_strings() {
        let mut v = vec![0x04, 0x08, b'['];
        v.extend(long(2));
        v.extend(str_node("勇者"));
        v.push(b'"');
        v.extend(long(4));
        v.extend_from_slice(&[0x78, 0x9C, 0xFF, 0xFE]);

        let blobs = walk_blobs(&v).unwrap();
        assert_eq!(blobs, vec![vec![0x78u8, 0x9C, 0xFF, 0xFE]]);
        assert_eq!(walk(&v, "x").unwrap().len(), 1, "普通通道只该吐那一条文本");
    }
}