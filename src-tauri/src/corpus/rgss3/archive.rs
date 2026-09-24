//! RGSS3A v3 归档读取器。
//!
//! 《勇战终章》这类游戏把整个 `Data/` 打包进 `Game.rgss3a`（975MB），
//! 所以「离线抽文本」的第一步是能从归档里按名字取文件。
//!
//! 算法移植自已验证过的 Python 探针（`.probe_rgss3/probe_rgss3a.py`）：
//!
//! ```text
//!   头 8 字节  "RGSSAD\0" + 版本(3)
//!   接着 4 字节 LE = seed_key，索引密钥 K = seed_key*9 + 3
//!   文件表每条 16 字节 [offset^K, size^K, file_key^K, name_len^K]（各 4 字节 LE）
//!     + name_len 字节文件名，逐字节异或 K 的第 (i%4) 个字节
//!     以 decoded offset == 0 的记录终止
//!   数据块每 4 字节异或 file_key，每块之后 file_key = file_key*7 + 3
//! ```
//!
//! **只读需要的那个文件**（seek + read），不整包解密 —— 975MB 全解一遍光是
//! 内存就撑不住，而离线抽取一次只碰几个 `Data/*.rvdata2`。
//!
//! 不是每个 RGSS3 游戏都打包：有的留松散 `Data/*.rvdata2`。两边长得不一样，
//! 但上层只需要「按名字读文件」，所以用 [`DataSource`] 把差异吃掉。

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// 归档魔数：`RGSSAD` + `\0`。
const MAGIC: &[u8] = b"RGSSAD\x00";
/// 只认 v3 —— VX Ace。RGSSAD v1/v2 是 XP/VX，数据布局不同，不能猜着读。
const VERSION: u8 = 3;

/// 文件表里的一条：数据在哪、多长、用什么密钥。
struct Entry {
    offset: u64,
    size: usize,
    /// 解密这个文件用的初始密钥，每条都不一样。
    key: u32,
}

/// 一个打开着的归档。文件表在打开时一次读进内存（4654 条，很小）。
pub struct Archive {
    file: File,
    /// 键是归一化过的名字（反斜杠换成正斜杠），方便上层按 `Data/X.rvdata2` 查。
    entries: HashMap<String, Entry>,
}

impl Archive {
    /// 打开归档并读完文件表。魔数/版本不符当场报错，不静默。
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)
            .map_err(|e| Error::new(format!("打不开 {}：{e}", path.display())))?;

        let mut head = [0u8; 12];
        file.read_exact(&mut head)
            .map_err(|e| Error::new(format!("{} 太小，不像 RGSS 归档：{e}", path.display())))?;
        if &head[..7] != MAGIC {
            return Err(Error::new(format!(
                "{} 不是 RGSS 归档（开头是 {:02X?}）",
                path.display(),
                &head[..7]
            )));
        }
        if head[7] != VERSION {
            return Err(Error::new(format!(
                "{} 是 RGSSAD v{}，wtool 只认 v3（VX Ace）",
                path.display(),
                head[7]
            )));
        }

        let seed = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
        let k = seed.wrapping_mul(9).wrapping_add(3);

        let mut entries = HashMap::new();
        loop {
            let mut hdr = [0u8; 16];
            file.read_exact(&mut hdr).map_err(|e| {
                Error::new(format!("{} 的文件表读到一半断了：{e}", path.display()))
            })?;
            let offset = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) ^ k;
            if offset == 0 {
                break; // 终止记录
            }
            let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) ^ k;
            let key = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]) ^ k;
            let name_len = (u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]) ^ k) as usize;

            let mut raw = vec![0u8; name_len];
            file.read_exact(&mut raw).map_err(|e| {
                Error::new(format!("{} 的文件名读到一半断了：{e}", path.display()))
            })?;
            let name: Vec<u8> = raw
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ ((k >> ((i % 4) * 8)) & 0xFF) as u8)
                .collect();

            entries.insert(
                String::from_utf8_lossy(&name).replace('\\', "/"),
                Entry {
                    offset: offset as u64,
                    size: size as usize,
                    key,
                },
            );
        }

        Ok(Self { file, entries })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// 读一个文件并解密。
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        let e = self
            .entries
            .get(name)
            .ok_or_else(|| Error::new(format!("归档里没有 {name}")))?;
        let (offset, size, fkey) = (e.offset, e.size, e.key);

        self.file.seek(SeekFrom::Start(offset))?;
        let mut blob = vec![0u8; size];
        self.file.read_exact(&mut blob)?;

        // 每 4 字节一块异或 file_key，每块之后密钥滚动。末尾不足 4 字节的
        // 最后一块只异或到实际长度为止（探针里是「补零解完再截断」，等价）。
        let mut key = fkey;
        for chunk in blob.chunks_mut(4) {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            let plain = (u32::from_le_bytes(word) ^ key).to_le_bytes();
            chunk.copy_from_slice(&plain[..chunk.len()]);
            key = key.wrapping_mul(7).wrapping_add(3);
        }
        Ok(blob)
    }

    /// `Data/` 下所有 `.rvdata2` 的名字（含 `Data/Map/Data/` 那一层）。
    fn list_data(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .entries
            .keys()
            .filter(|n| n.starts_with("Data/") && n.ends_with(".rvdata2"))
            .cloned()
            .collect();
        out.sort(); // HashMap 的遍历顺序不定，排一下让扫描结果可复现
        out
    }
}

/// RPG 数据从哪来：打包的归档，还是散着的目录。
pub enum DataSource {
    Packed(Archive),
    /// 松散模式：记游戏根目录，`Data/X` 直接落到磁盘上。
    Loose(PathBuf),
}

impl DataSource {
    /// 按游戏目录挑一种：有 `Game.rgss3a` 就归档，否则看有没有 `Data/`。
    /// 两种都没有就是目录填错了，当场说清楚。
    pub fn open(game_dir: &Path) -> Result<Self> {
        let packed = game_dir.join("Game.rgss3a");
        if packed.is_file() {
            return Ok(DataSource::Packed(Archive::open(&packed)?));
        }
        if game_dir.join("Data").is_dir() {
            return Ok(DataSource::Loose(game_dir.to_path_buf()));
        }
        Err(Error::new(format!(
            "{} 下既没有 Game.rgss3a 也没有 Data/ 目录，游戏目录是不是填错了？",
            game_dir.display()
        )))
    }

    pub fn contains(&self, name: &str) -> bool {
        match self {
            DataSource::Packed(a) => a.contains(name),
            DataSource::Loose(root) => loose_path(root, name).is_file(),
        }
    }

    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        match self {
            DataSource::Packed(a) => a.read(name),
            DataSource::Loose(root) => {
                let path = loose_path(root, name);
                std::fs::read(&path)
                    .map_err(|e| Error::new(format!("读不了 {}：{e}", path.display())))
            }
        }
    }

    /// `Data/` 下所有 `.rvdata2` 的名字，两边统一用正斜杠。
    pub fn list_data(&self) -> Vec<String> {
        match self {
            DataSource::Packed(a) => a.list_data(),
            DataSource::Loose(root) => {
                let mut out = Vec::new();
                walk_loose(&root.join("Data"), root, &mut out);
                out.sort();
                out
            }
        }
    }
}

/// 把 `Data/a/b.rvdata2` 这种名字拼成磁盘路径（一段一段 join，不手拼分隔符）。
fn loose_path(root: &Path, name: &str) -> PathBuf {
    name.split('/').fold(root.to_path_buf(), |p, seg| p.join(seg))
}

/// 递归收集松散目录里的 `.rvdata2`，名字相对游戏根。
fn walk_loose(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            walk_loose(&path, root, out);
        } else if path.extension().and_then(|x| x.to_str()) == Some("rvdata2") {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 目标游戏的归档：只读文件表（几十 KB），**不读那 975MB**。
    ///
    /// 这条测试依赖本机装着那个游戏，所以归档不在就直接跳过 —— 换个机器跑
    /// 测试不该红。文件数 4654 是探针跑出来的实测量，游戏换版本时会变，
    /// 那时改这个数就行（改数之前先想想是不是解密算法真的漂了）。
    const PACKED: &str = r"D:\myProgram\game2\yongzhanrpgzhongzhang360\Game.rgss3a";

    #[test]
    fn reads_real_archive_file_table() {
        let path = Path::new(PACKED);
        if !path.is_file() {
            eprintln!("跳过：本机没有 {PACKED}");
            return;
        }
        let a = Archive::open(path).unwrap();
        assert_eq!(a.entries.len(), 4654, "文件表条数和探针不一致");

        for name in [
            "Data/Scripts.rvdata2",
            "Data/CommonEvents.rvdata2",
            "Data/DataEx.rvdata2",
        ] {
            assert!(a.contains(name), "归档里少了 {name}");
        }
        assert!(
            a.list_data().iter().any(|n| n.ends_with("Map001.rvdata2")),
            "一张地图都没列出来"
        );
    }

    /// 不是归档的文件要报错，不能当空的读下去。
    #[test]
    fn rejects_non_archive() {
        let dir = std::env::temp_dir().join(format!("wtool_archive_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let bad = dir.join("Game.rgss3a");
        std::fs::write(&bad, b"not an archive at all").unwrap();
        let err = match Archive::open(&bad) {
            Ok(_) => panic!("坏文件不该被当成归档打开"),
            Err(e) => e,
        };
        assert!(err.message.contains("不是 RGSS 归档"), "{}", err.message);
    }
}