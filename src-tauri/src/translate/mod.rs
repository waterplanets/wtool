//! 翻译层：把语料里的原文喂给 LLM（OpenAI 兼容接口），产出一张 wtool 词典。
//!
//! 这是整个翻译功能的收口一步，把第 1（格式）、第 2（语料）步的东西连起来：
//!
//! ```text
//!  Corpus                              wtool 词典文件
//! (第 2 步集来的原文)                   (第 1 步能读、游戏能查的表)
//!    │  flat() 摊平                         ▲
//!    ▼                                      │ write_tsv 增量落盘
//!  过滤：resume 已翻的跳过 ──► 分批 ──► mask ──► LLM ──► unmask ──► dict
//!                                   (batch.rs)  (openai.rs)  (mask.rs)
//! ```
//!
//! 三件事在这里定型：
//!
//! - **只翻没翻过的**：`resume` 让调用方告诉它「哪些原文已经在目标词典里」，
//!   重跑时不重翻，断点续传就是这么来的。
//! - **每批落盘**：一批翻完立刻合并进目标文件。死在任一批，重跑从断处接着翻。
//! - **翻译失败就报错**：不静默跳过，也不吞 —— 反正前面成功的批已落盘。

pub mod batch;
pub mod gtx;
pub mod mask;
pub mod openai;
pub mod prompt;

use std::collections::BTreeMap;

use crate::corpus::Corpus;
use crate::error::Result;
use mask::{mask, unmask};

/// 单条原文超过这个字符数就当是机器数据（真对话没那么长），不喂给模型。
const MAX_SRC_LEN: usize = 400;

/// 一个翻译后端：一次吃一批（屏蔽控制码后的）原文，吐回【与输入一一对应】的译文。
///
/// 实现端各自负责连自己的接口（LLM 的 `chat/completions`、gtx 的网页接口…），
/// 上层 [`translate`] 只调它、按序把译文拼回原文。条数/顺序对不上由实现端自己
/// 报错（见 [`openai`] 里「宁可整批重来，不能按下标错位」的约定）。
pub trait TranslateBackend {
    fn translate_batch(&self, masked: &[String]) -> Result<Vec<String>>;
}

/// 把语料翻成一张 `<原文 → 译文>` 的词典。
///
/// - `corpus`：要翻的原文集合（第 2 步产物）。
/// - `backend`：翻译后端（gtx / LLM 都实现了 [`TranslateBackend`]，这里不关心是谁）。
/// - `batch`：每批喂几条原文（`0` 视为整块一批）。
/// - `resume`：`Fn(&str) -> bool`，返回 `true` 的原文视为「已翻过」，跳过不翻。
/// - `report`：进度回调，每批用字符串打一行。
/// - `flush`：每批结束后，把**当前全部的**译文交出去持久化（调用方决定写哪儿）。
///   失败要上报（返回 Err），好在死点前成功的批已经落盘，重跑时 `resume` 让它们被
///   跳过 —— 这是「断点续传」真正的接线点。
pub fn translate(
    corpus: &Corpus,
    backend: &dyn TranslateBackend,
    batch: usize,
    resume: &dyn Fn(&str) -> bool,
    report: &mut dyn FnMut(&str),
    flush: &mut dyn FnMut(&BTreeMap<String, String>) -> Result<()>,
) -> Result<BTreeMap<String, String>> {
    let flat = corpus.flat();
    // 语料里会混进「机器文本」：插件的 JS 注释、整块 JSON 数据、超长数据行。
    // 这些不是玩家看的字，喂给 LLM 会被要求「翻译一大坨代码」→ 生成超长输出，
    // 又贵又往回污染词典。所以进 LLM 之前先粗暴筛掉：带 `/*` C 注释标记的，
    // 或超过 [`MAX_SRC_LEN`] 个字符的超长行。
    let mut skipped_noise = 0usize;
    let candidates: Vec<String> = flat
        .into_iter()
        .map(|(_, s)| s)
        .filter(|s| !resume(s))
        .filter(|s| {
            if s.contains("/*") || s.chars().count() > MAX_SRC_LEN {
                skipped_noise += 1;
                false
            } else {
                true
            }
        })
        .collect();
    if skipped_noise > 0 {
        report(&format!(
            "过滤掉 {skipped_noise} 条代码/超长机器文本（/* */ 或 >{MAX_SRC_LEN} 字符），不喂给模型"
        ));
    }

    report(&format!(
        "待翻译 {} 条（{} 条已翻过，跳过）",
        candidates.len(),
        corpus.unique().saturating_sub(candidates.len())
    ));

    let mut dict: BTreeMap<String, String> = BTreeMap::new();
    let total = candidates.len();
    let mut done = 0usize;

    for batch in batch::chunk(&candidates, batch) {
        let srcs: Vec<&String> = batch.iter().collect();
        let skipped = translate_chunk(&srcs, backend, &mut dict, report)?;
        done += srcs.len() - skipped;
        report(&format!("已翻 {done}/{total}（本轮跳过 {skipped} 条）"));
        flush(&dict)?;
    }

    Ok(dict)
}

/// 翻一小片原文，返回本片跳过（没能翻成）的条数。
///
/// 翻不好的批次不报错中止：先整体试一遍，若整批连着超时/失败，就**劈两半递归**
/// 各自重试，直到定位到那条「病原文」单独跳过——这样一条让模型卡住的文本，
/// 再也不会拖死整个任务，其余条目照样落盘。
fn translate_chunk(
    srcs: &[&String],
    backend: &dyn TranslateBackend,
    dict: &mut BTreeMap<String, String>,
    report: &mut dyn FnMut(&str),
) -> Result<usize> {
    // 掩码：控制码换占位符，模型只翻真话。
    let meta: Vec<(&String, String, Vec<String>)> = srcs
        .iter()
        .map(|s| {
            let (m, o) = mask(s);
            (*s, m, o)
        })
        .collect();
    let masked_input: Vec<String> = meta.iter().map(|x| x.1.clone()).collect();

    // 单批调用带重试；重试的动静单独攒一行，成功后统一并入进度。
    let mut retry_lines = Vec::new();
    match batch::call_with_retry(
        &mut || backend.translate_batch(&masked_input),
        &mut |m| retry_lines.push(m.to_string()),
    ) {
        Ok(translated) => {
            for line in &retry_lines {
                report(line);
            }
            let mut skipped = 0usize;
            // 译文换回控制码，写进词典。unmask 对不上占位符就跳过这一条
            // （错的控制码宁可不写，也不能写错的进游戏）。
            for ((src, _, originals), translated) in meta.iter().zip(translated.iter()) {
                match unmask(translated, originals) {
                    Ok(restored) if !restored.trim().is_empty() => {
                        dict.insert((*src).to_string(), restored);
                    }
                    Ok(_) => skipped += 1,
                    Err(e) => {
                        skipped += 1;
                        report(&format!("跳过 1 条（还原失败）：{e}"));
                    }
                }
            }
            Ok(skipped)
        }
        Err(e) if srcs.len() == 1 => {
            // 单条仍失败：病根就是它。记录原因并跳过——但不静默吞，把失败理由
            // 打出来，方便查它到底是模型返回乱码、还是超时/限流。
            let show: String = srcs[0].chars().take(40).collect();
            report(&format!("跳过顽固原文（{e}）：{show}"));
            Ok(1)
        }
        Err(_) => {
            // 整批失败：劈两半各自递归，既能定位病文本，又保证其余照翻。
            let mid = srcs.len() / 2;
            report(&format!("整批 {} 条请求连续失败，劈半重试…", srcs.len()));
            Ok(translate_chunk(&srcs[..mid], backend, dict, report)?
                + translate_chunk(&srcs[mid..], backend, dict, report)?)
        }
    }
}