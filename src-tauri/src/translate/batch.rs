//! 分批与失败重试。
//!
//! LLM 一次请求能处理的条数有限（太多会截断、错位），所以要切成一批批翻；
//! 网络/接口偶发失败就重试几次。重试是**次数有限 + 退避**的：翻完的批已经写进
//! 目标文件，就算这一批最终还是失败、整个任务报错退出，重跑时断点续传会跳过
//! 成功落盘的批，从失败的这一批接着来——所以失败不需要吞掉，直接报错即可。

use std::time::Duration;

use crate::error::{Error, Result};

/// 单批最多重试几次。翻译这种长任务网络难免抖动，但重试次数不能太多——
/// 一次「渲染慢/卡住的请求」每档要等一个超时周期，档位越多，静止看起来越久。
/// 3 次（首试 + 3 重）配合 15s 级超时，最坏也就一分钟内见分晓，足以扛住偶发抖动，
/// 又不至于让人觉得「卡死」。
pub const MAX_RETRIES: u32 = 3;
/// 首次重试前等的毫秒数；之后每次翻倍（封顶 [`MAX_RETRY_DELAY_MS`]，别等上天）。
pub const RETRY_DELAY_MS: u64 = 1000;
/// 单次重试等待的上限。
pub const MAX_RETRY_DELAY_MS: u64 = 20000;

/// 把一个切片切成 `size` 大小的等份。`size=0` 视为整块一批。
pub fn chunk<T>(items: &[T], size: usize) -> Vec<&[T]> {
    if size == 0 {
        return vec![items];
    }
    (0..items.len())
        .step_by(size)
        .map(|start| &items[start..(start + size).min(items.len())])
        .collect()
}

/// 调一次翻译请求（`call`），失败就退避重试，直到达到 [`MAX_RETRIES`] 次数。
/// `report` 只在真正重试时给一行话（成功或直接失败不打扰）。
///
/// `call` 收 `FnMut`（不是 `Fn`）：调用方想在闭包里攒点重试上下文（测试里数
/// 次数）时不至于被卡住；真正生产里只是读一批 masked 原文、没副作用。
pub fn call_with_retry<R>(
    call: &mut dyn FnMut() -> Result<R>,
    report: &mut impl FnMut(&str),
) -> Result<R> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match call() {
            Ok(v) => return Ok(v),
            Err(e) if attempt <= MAX_RETRIES => {
                let delay = (RETRY_DELAY_MS * 2u64.pow(attempt - 1)).min(MAX_RETRY_DELAY_MS);
                report(&format!(
                    "请求失败（第 {attempt}/{MAX_RETRIES} 次重试，等 {delay}ms）：{e}"
                ));
                std::thread::sleep(Duration::from_millis(delay));
            }
            Err(e) => {
                return Err(Error::new(format!(
                    "重试 {MAX_RETRIES} 次仍失败，本轮中止（已完成的部分已落盘，重跑会接着翻）：{e}"
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_splits_evenly() {
        let v = [1, 2, 3, 4, 5, 6];
        let parts = chunk(&v, 2);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], &[1, 2]);
        assert_eq!(parts[2], &[5, 6]);
    }

    #[test]
    fn chunk_handles_remainder() {
        let v = [1, 2, 3, 4, 5];
        let parts = chunk(&v, 2);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[2], &[5]);
    }

    #[test]
    fn chunk_size_zero_means_one_whole_batch() {
        let v = [1, 2, 3];
        let parts = chunk(&v, 0);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], &[1, 2, 3]);
    }

    #[test]
    fn empty_slice_yields_no_batches() {
        let v: [i32; 0] = [];
        assert!(chunk(&v, 5).is_empty());
    }

    /// 第一次失败 → 重试后成功，只报一次重试消息。
    #[test]
    fn retry_succeeds_after_first_failure() {
        let mut calls = 0;
        let mut reports = Vec::new();
        let out = call_with_retry(
            &mut || {
                calls += 1;
                if calls == 1 {
                    Err(Error::new("网络抖了一下".to_string()))
                } else {
                    Ok::<i32, _>(7)
                }
            },
            &mut |m| reports.push(m.to_string()),
        )
        .unwrap();
        assert_eq!(out, 7);
        assert_eq!(reports.len(), 1);
    }

    /// 一直失败 → 到达上限报错。
    #[test]
    fn retry_gives_up_after_limit() {
        let mut reports = Vec::new();
        let r = call_with_retry(
            &mut || Err::<i32, _>(Error::new("一直失败".to_string())),
            &mut |m| reports.push(m.to_string()),
        );
        assert!(r.is_err());
        // 总共试了 1 + MAX_RETRIES 次（首试 + 上限次重试）
        assert_eq!(reports.len(), MAX_RETRIES as usize);
    }
}