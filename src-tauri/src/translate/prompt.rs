//! 提示词组装。
//!
//! 一次请求 = 一个 system 指令（翻译怎么翻、占位符怎么保）+ 一个 user 消息
//! （这一批要翻的原文，序列化成 JSON 数组）。让模型把整批的译文**按同样的顺序**
//! 以 `{"translations": [...]}` 返回，wtool 这边数组下标一对就对上原文，不用在
//! 提示词里逐条编号。

use serde::Serialize;

/// system 指令的措辞。关键三件事：翻什么语（RPG 对白、自然中文）、
/// 别动结构（行数不变）、占位符 `[[N]]` 是护身符勿碰勿译勿挪位。
///
/// 输出要求用「对象包一个数组」而不是裸数组：DeepSeek 的 `response_format:
/// json_object` 只允许顶层是对象，硬要裸数组会被拒。用户消息是把 masked 原文
/// 作为数组给过去（[`user_message`]），模型按同样顺序回 `{"translations": [...]}`。
pub fn system_prompt() -> &'static str {
    "你是一位专业的 RPG Maker 游戏日语→简体中文翻译者。\
\n规则：\
\n1. 只翻译内容文字，保持原本的行数和换行结构，不要增删行。\
\n2. 文本中的 [[0]]、[[1]] 等占位符代表游戏控制码，必须原样保留在译文中的对应位置，不要改写、不要翻译、不要移动顺序。\
\n3. 译文要自然、口语化，贴合游戏对白和世界观，专有名词可保留日文原文。\
\n4. 只输出一个 JSON 对象：{\"translations\": [按输入顺序逐条对应每一条原文的译文数组]}，不要输出任何多余文字、不要用 Markdown 代码块、不要有结尾解释。\
\n5. 不允许把两条合并成一条，也不允许拆分一条，translations 的元素数必须与输入条数完全一致。"
}

/// 把这一批 masked 原文序列化成 user 消息的 JSON 数组文本。
pub fn user_message(masked: &[String]) -> String {
    #[derive(Serialize)]
    struct Body<'a>(&'a [String]);
    serde_json::to_string(&Body(masked)).expect("String 数组一定能序列化")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// user 消息必须是一整个能解析的 JSON 数组，元素数与输入一致。
    #[test]
    fn user_message_is_json_array_with_matching_length() {
        let src = vec!["こんにちは".to_string(), "[[0]]攻撃".to_string()];
        let msg = user_message(&src);
        let v: serde_json::Value = serde_json::from_str(&msg).expect("必须是合法 JSON");
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), src.len());
        assert_eq!(v[0], "こんにちは");
    }
}