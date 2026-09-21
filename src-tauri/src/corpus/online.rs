//! 在线采集：翻译任务跑在「采集模式」时，游戏侧推上来的文本被落进语料文件，
//! 同时照常回空 map（游戏保持原文）—— 这就是「攒料」而不「翻」。
//!
//! 和离线扫描的分工：离线扫 `data/*.json` + `plugins.js`，拿到的是静态料；
//! 在线采集拿到的是**玩家真正玩到的动态料**——脚本拼出来的消息（`$DU` 那批）、
//! `\v[n]` 展开后的真话、以及离线扫不到的运行时文本。两头的原文合并进同一个
//! [`Corpus`]，第 3 步喂 LLM 时从那里取。
//!
//! 桥是照着「翻」设计的（问一批、回 map、改写游戏内存），这里不改桥，只改
//! wtool 侧的行为：任务带 `collect` 标志起跑时，把 `Text` / `Terms` 事件的原文
//! 追加进语料，`apply_terms` 回一个空 map —— 游戏侧收到替换 0 条，等于没动。

use std::path::PathBuf;

use crate::corpus::{self, Corpus, Granularity};
use crate::error::Result;

/// 采集器：拼内存里的语料 + 落盘路径。每次事件把原文收进去，定时写盘，
/// 保证玩到一半被强杀也不至于把已经玩到的料全丢。
pub struct Collector {
    path: PathBuf,
    corpus: Corpus,
}

impl Collector {
    /// 开一个采集器。目标文件已存在就读回来续写（在线采集跨多次会话累计），
    /// 不存在就从空开始。
    pub fn open(path: PathBuf) -> Result<Self> {
        let corpus = if path.is_file() {
            corpus::load(&path)?
        } else {
            Corpus::new()
        };
        Ok(Self { path, corpus })
    }

    /// 一段对话（`\v[n]` 已在游戏侧展开），收整段 + 逐行。
    ///
    /// 整段只在**真有多行**时收 —— 单行对话的「整段」和「单行」是同一个串，
    /// 两处各收一遍只会让第 3 步把同一句翻两遍（和离线侧的判定一致）。
    pub fn on_text(&mut self, text: &str) {
        if text.contains('\n') {
            self.corpus.push(Granularity::Whole, text);
        }
        for line in text.split('\n') {
            self.corpus.push(Granularity::Line, line);
        }
    }

    /// 一批整表原文。按 `label` 认片段（窗口文本那批，菜单项整串画），
    /// 其余看粒度：含 `\n` 的按整段，否则按单行。
    pub fn on_terms(&mut self, label: &str, loose: bool, texts: &[String]) {
        let fragment = label.contains("窗口");
        for t in texts {
            let g = if fragment {
                Granularity::Fragment
            } else if loose && t.contains('\n') {
                Granularity::Whole
            } else {
                Granularity::Line
            };
            self.corpus.push(g, t);
        }
    }

    /// 写盘，返回写进去多少条（去重后）。
    pub fn save(&mut self) -> Result<usize> {
        corpus::save(&self.path, &self.corpus)
    }

    /// 三个粒度各自的去重条数。
    pub fn per_granularity(&self) -> (usize, usize, usize) {
        self.corpus.per_granularity()
    }

    /// 「原始 N 条，去重后 M 条（…）」。
    pub fn stats(&self) -> String {
        self.corpus.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wtool_online_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("corpus.tsv")
    }

    /// 一段对话：整段 + 逐行都收，且去重正确。
    #[test]
    fn on_text_records_whole_and_lines() {
        let mut c = Collector::open(scratch("text")).unwrap();
        c.on_text("こんにちは世界\n二行目です");
        assert_eq!(c.stats(), "原始 3 条，去重后 3 条（整段 1 / 单行 2 / 片段 0）");
    }

    /// 窗口文本那批按 Fragment 收；纯 ASCII 的整批（如「HP」）该被过滤。
    #[test]
    fn on_terms_classifies_window_as_fragment_and_filters_ascii() {
        let mut c = Collector::open(scratch("terms")).unwrap();
        c.on_terms("窗口文本", true, &["\\I[188]手帳".into()]);
        c.on_terms("窗口文本", true, &["HP".into()]);
        assert_eq!(c.per_granularity(), (0, 0, 1));
    }

    /// 续写：开一个已有文件，日志里的原文能累计。
    #[test]
    fn open_resumes_and_saves_roundtrip() {
        let path = scratch("resume");
        {
            let mut c = Collector::open(path.clone()).unwrap();
            c.on_text("一行目\n二行目");
            c.save().unwrap();
        }
        let mut c = Collector::open(path).unwrap();
        c.on_text("新しく出た行");
        c.save().unwrap();
        // 上次收的整段(1) + 上次两行 + 这次一行 = 整段 1 / 单行 3
        assert_eq!(c.per_granularity(), (1, 3, 0));
    }
}