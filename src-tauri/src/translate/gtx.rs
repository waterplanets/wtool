//! 调 Google 网页翻译接口（`translate.googleapis.com/translate_a/single`，
//! `client=gtx`）翻一条 masked 原文。
//!
//! 免费、无 key、质量对齐 MTool（它走的也是这一类公共网页接口），是 wtool 默认的
//! 省钱后端。代价是「有求必应」要节制：一次请求只能翻一条，翻得太多、太密会被
//! IP 临时限速（429）——所以在两条之间打一个 [`THROTTLE`] 的隔，把请求匀开。
//! 万一还是被限，上层 [`call_with_retry`] 的退避会接手重试。
//!
//! 用到的 `reqwest::Url::parse_with_params` 只是 reqwest 自己带出来的，没新引入
//! 第三方 crate。

use std::time::Duration;

use crate::error::{Error, Result};
use crate::translate::TranslateBackend;
use reqwest::blocking::Client;

/// 每翻一条之间的间隔：把请求匀开，太密谷歌会 429。
const THROTTLE: Duration = Duration::from_millis(300);

/// gtx 后端：一次翻一条原文，保证「返回顺序与输入一致」。
///
/// `translate` 层已经把一批原文打散成多次单条请求，这里不必自己再拆。
pub struct Backend;

impl Backend {
    pub fn new() -> Self {
        Backend
    }
}

impl TranslateBackend for Backend {
    fn translate_batch(&self, masked: &[String]) -> Result<Vec<String>> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            // 浏览器同款 UA，别让它把我们当脚本拒掉。
            .user_agent("Mozilla/5.0")
            .build()?;

        let mut out = Vec::with_capacity(masked.len());
        for (i, line) in masked.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(THROTTLE);
            }
            out.push(translate_line(&client, line)?);
        }
        Ok(out)
    }
}

/// 翻一条。接口返回形如：
///
/// ```json
/// [ [["译文","原文",null,null,...], ["译文2","原文2",...], ...], ["整段原文"], ... ]
/// ```
///
/// 译文 = 第一个数组里每段的 `[0]` 脚标按序拼接。
fn translate_line(client: &Client, text: &str) -> Result<String> {
    let url = reqwest::Url::parse_with_params(
        "https://translate.googleapis.com/translate_a/single",
        &[
            ("client", "gtx"),
            ("sl", "ja"),
            ("tl", "zh-CN"),
            ("dt", "t"),
            ("q", text),
        ],
    )
    .map_err(|e| Error::new(format!("拼 gtx 请求地址失败：{e}")))?;

    let resp = client.get(url).send()?.error_for_status()?;
    let v: serde_json::Value = resp.json()?;

    let segs = v
        .get(0)
        .and_then(|a| a.as_array())
        .ok_or_else(|| Error::new(format!("gtx 返回格式非预期：{v}")))?;

    let joined = segs
        .iter()
        .filter_map(|seg| seg.get(0).and_then(|t| t.as_str()))
        .collect::<String>();

    Ok(joined)
}

#[cfg(test)]
mod tests {
    /// 手动构造 gtx 响应的形状（含整段原文、含 null 段），拼接必须只看 `[0]` 且按序。
    #[test]
    fn joins_first_elements_in_order() {
        // translate_line 需要网络，这里不走真接口 —— 只用一个假 Client 会构建失败，
        // 所以代码里不测整条线；这个用例守卫「拼接逻辑」的片段：构造与响应同构的
        // JSON，抽出 `[0]` 脚标。
        let v: serde_json::Value = serde_json::from_str(
            r#"[ [["你好","こんにちは",null,null,1]], ["こんにちは"], "ja" ]"#,
        )
        .unwrap();
        let segs = v.get(0).and_then(|a| a.as_array()).unwrap();
        let joined: String = segs
            .iter()
            .filter_map(|seg| seg.get(0).and_then(|t| t.as_str()))
            .collect();
        assert_eq!(joined, "你好");
    }

    /// `q` 参数里有中文/控制码占位符也能被 URL 编码，不破坏请求地址构造。
    #[test]
    fn builds_url_with_encoded_query() {
        let url = reqwest::Url::parse_with_params(
            "https://translate.googleapis.com/translate_a/single",
            &[("q", "こんにちは[[0]]世界")],
        )
        .unwrap();
        assert_eq!(
            url.query().unwrap(),
            "q=%E3%81%93%E3%82%93%E3%81%AB%E3%81%A1%E3%81%AF%5B%5B0%5D%5D%E4%B8%96%E7%95%8C"
        );
    }
}