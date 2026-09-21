//! 调 OpenAI 兼容的 `chat/completions` 接口，翻一批 masked 原文。
//!
//! 这里 deps 了什么：reqwest 做 HTTP。它不是新引入的第三方 —— tauri 本来就把
//! reqwest 带在依赖树里（tauri 内部用它做网络请求），我们只是显式声明启用
//! `blocking` + `json` 两个 feature 直接复用它。
//!
//! 全程同步（阻塞）：翻译跑在 wtool 的 worker 线程里，阻塞一整个网络的往返
//! 最直白，绕开 tauri 的 async_runtime，出错重试也简单。
//!
//! 返回的是「和输入一一对应」的一组译文。**数量对不上输入就报错** —— 一个萝卜
//! 一个坑，宁可整个批重来，也不能按下标错位地把译文接到别的原文上。

use std::sync::mpsc;
use std::time::Duration;

use crate::error::{Error, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};

/// 调度方给一次请求多久。再久就当它挂了。**拖长到 45s**：输出已被 `max_tokens`
    /// 兜底，宁可等一个真正健康但偏慢的请求拿结果，也不要一次次掐断重发——掐断
    /// 不撤销生成，钱会重烧。45s 已足够挡掉「挂死」类故障，又不至于静默等太久。
    const DEADLINE_SECS: u64 = 45;

/// LLM 后端的连接配置，实现 [`crate::translate::TranslateBackend`] 供上层统一调度。
///
/// base/model/key 三个字段分别对应接口基址、模型名、token；跑 gtx 时用不上它。
#[derive(Debug, Clone)]
pub struct Backend {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

impl crate::translate::TranslateBackend for Backend {
    fn translate_batch(&self, masked: &[String]) -> Result<Vec<String>> {
        translate_batch(&self.base_url, &self.model, &self.api_key, masked)
    }
}

/// 给一批 masked 原文做一次翻译请求，返回数量、顺序都与输入一致的译文。
///
/// `url` 是接口基址（如 `https://api.deepseek.com`），拼接一个 `/chat/completions`。
pub fn translate_batch(
    url: &str,
    model: &str,
    api_key: &str,
    masked: &[String],
) -> Result<Vec<String>> {
    let endpoint = format!("{}/chat/completions", url.trim_end_matches('/'));

    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": crate::translate::prompt::system_prompt() },
            { "role": "user", "content": crate::translate::prompt::user_message(masked) }
        ],
        // 押低一点，翻出来的字面更贴原文、不会自己发挥。
        "temperature": 0.2,
        // DeepSeek 支持 json_object 模式，保证返回是合法 JSON，节省解析功夫。
        "response_format": { "type": "json_object" },
        // 掐死单次请求的输出上限。翻译用不了太多 token；万一某条原文触发模型
        // 狂输出（实测会烧钱：掐断请求但不撤销已生成的 token 计费），这是一道
        // 硬闸，保证单次耗费封顶，而不是无限烧。
        "max_tokens": 4096,
    });

    // reqwest 的 timeout 在「连接层挂死」时不一定可靠（实测会卡到没反应），所以
    // 额外加一道硬性看门狗：请求挪进 worker 线程跑，主线程只等 DEADLINE_SECS，
    // 到点没收到结果就按超时中止——宁可整批重试，也绝不静默冻住（上层重试会把
    // 「请求超时」写进日志，用户看得到正在动）。
    let client = Client::new();
    let api_key = api_key.to_string();
    let json: Value = request_with_deadline(
        move || {
            let resp = client
                .post(&endpoint)
                .bearer_auth(&api_key)
                .json(&body)
                .send()?
                .error_for_status()?;
            Ok(resp.json()?)
        },
        DEADLINE_SECS,
    )?;

    let content = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|ch| ch.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Error::new(format!(
                "模型返回里找不到 choices[0].message.content（原样见下）：\n{}",
                json
            ))
        })?;

    let list = extract_list(content)?;
    if list.len() != masked.len() {
        return Err(Error::new(format!(
            "模型的译文条数({}) 和输入条数({}) 对不上 —— 防止错位，整批作废，稍后整批重翻。",
            list.len(),
            masked.len()
        )));
    }
    Ok(list)
}

/// 在专用线程里执行一次 HTTP 请求，主线程只等 `secs` 秒。
///
/// reqwest 的 `.timeout()` 在连接层挂死时可能不兜底（实测过），这一层是对它的
/// 硬补充：到点没收齐结果就报“超时”，让上层重试 + 落日志。超时的那个 worker
/// 线程会自己慢慢结束，不阻塞主任务。
fn request_with_deadline(
    call: impl FnOnce() -> Result<Value> + Send + 'static,
    secs: u64,
) -> Result<Value> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // 发送失败（接收端已因超时离开）也无妨，忽略即可。
        let _ = tx.send(call());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .map_err(|_| Error::new(format!("请求在 {secs}s 内没返回，按超时中止（整批将重试）")))?
}

/// 给一批 masked 原文做一次翻译请求，返回数量、顺序都与输入一致的译文。
///
/// 理想情况 content 是 `{"translations": [...]}`；个别端点/模型可能给裸数组，
/// 或者（中文大模型偶尔）在 JSON 外面裹一层 ```` ```json ```` 代码围栏 —— 这里
/// 都容一脚。规则：
///
/// 1. 先剥掉首尾的 Markdown 代码围栏与空白；
/// 2. 解析成一个 JSON value；
/// 3. 直接是数组 → 用它；是对象 → 取 `.translations`（大小写不敏感）；都拿不到
///    就是接口返错格式，报错。
fn extract_list(content: &str) -> Result<Vec<String>> {
    let trimmed = strip_fence(content);
    let v: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        Error::new(format!(
            "模型的 content 不是合法 JSON（{e}）。原文：\n{trimmed}"
        ))
    })?;

    let arr = match &v {
        serde_json::Value::Array(a) => Some(a),
        serde_json::Value::Object(o) => o
            .iter()
            .find(|(k, val)| k.eq_ignore_ascii_case("translations") && val.is_array())
            .map(|(_, val)| val.as_array().unwrap()),
        _ => None,
    }
    .ok_or_else(|| Error::new(format!("content 里没有译文数组。原文：\n{trimmed}")))?;

    arr.iter()
        .map(|x| x.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| Error::new(format!("译文数组里有不是字符串的元素。原文：\n{trimmed}")))
}

/// 剥掉首尾空白，以及可能存在的 Markdown 代码围栏（模型偶尔会裹一层
/// ```` ```json … ``` ````）。剥完仍回到一个干净能解析的 JSON 片段。
fn strip_fence(s: &str) -> &str {
    let mut t = s.trim();
    if t.starts_with("```") {
        // 去掉开头的围栏标记行（``` 以及可能紧跟的 json 字样）
        t = &t[3..];
        t = t.trim_start();
        for w in ["json", "JSON", "Json"] {
            if t.starts_with(w) {
                t = &t[w.len()..];
                t = t.trim_start();
                break;
            }
        }
        if let Some(rest) = t.strip_suffix("```") {
            t = rest.trim_end();
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fences_and_whitespace() {
        assert_eq!(strip_fence("\n```json\n  {\"a\":1}\n```\n"), "{\"a\":1}");
        assert_eq!(strip_fence("  [1,2]  "), "[1,2]");
        assert_eq!(strip_fence("{\"a\":1}"), "{\"a\":1}");
    }

    #[test]
    fn extract_object_translations() {
        let c = "{\"translations\":[\"你好\",\"再见\"]}";
        assert_eq!(extract_list(c).unwrap(), ["你好", "再见"]);
    }

    #[test]
    fn extract_bare_array() {
        let c = "[\"你好\"]";
        assert_eq!(extract_list(c).unwrap(), ["你好"]);
    }

    #[test]
    fn extract_rejects_bad_shape() {
        assert!(extract_list("{\"foo\": 1}").is_err());
        assert!(extract_list("not json at all").is_err());
    }
}