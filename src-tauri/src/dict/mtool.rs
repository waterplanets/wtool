//! MTool 词典（`TrsData.bin`）适配器 —— 防腐层。

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::Result;
use crate::model::TsSentence;

/// 读取词典文件，转成内部统一模型。
///
/// 之所以不直接返回 `HashMap<String, String>`：
/// MTool 的格式是「不由我们定义的外部契约」，一旦泄漏到上层，
/// 将来换格式就要动全工程。这里多做一步转换，把脏东西挡在外面。
pub fn load_ts_file(path: &Path) -> Result<Vec<TsSentence>> {
    let raw: String = std::fs::read_to_string(path)?;
    let map: BTreeMap<String, String> = serde_json::from_str(&raw)?;

    Ok(map
        .into_iter()
        .map(|(source, target)| TsSentence { source, target })
        .collect())
}
