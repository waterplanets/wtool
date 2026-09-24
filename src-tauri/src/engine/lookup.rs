//! 同步查表通道 —— 让游戏在「画字那一刻」就拿到译文，而不是先画原文、等回话到了再重画。
//!
//! 为什么不并进 `link` 那条通道，而是单独一个端口、单独一个线程：
//!
//! - **时延**：`link` 那条通道由翻译主循环每 [`crate::task`] 的 `TICK`（100ms）
//!   才转一圈。游戏要是阻塞在绘制里等回话，就得白等一整个 tick —— 那就不是
//!   「同步」了，是「每次画字卡 100ms」。这条通道由一个线程**阻塞在读上**，
//!   收到就查、查完就回，不经过任何循环。
//! - **不被大写入堵住**：`link` 那条还得传菜单整表（几百 KB）。跟同步查表挤
//!   同一条 socket，那几笔大写入会把「一问一答」拖后。分开之后互不干扰。
//! - **形状本来就不同**：`link` 是「整批问、整批答」的事件流；这条是
//!   「一问一答、必须对得上号」。硬塞进同一个 `Engine` 里只会让两边都变脏。
//!
//! 线格式：一条一行 JSON，UTF-8，`\n` 结尾。两个方向的形状不一样，不会认错：
//!
//! ```text
//! 游戏 → wtool   {"t":"q","id":7,"text":"原文"}
//! wtool → 游戏   {"t":"a","id":7,"hit":true,"text":"译文"}
//! ```
//!
//! 查不到时必须回 `hit:false`（且不带 `text`）—— 游戏侧正阻塞等着，
//! 不回话等于把游戏冻到超时。
//!
//! 这个模块只做「收一条、查一条、回一条」，查表用的是 [`Dict::translate`]
//! 的三级退级 —— 必须和整批那条路用同一套查法，否则同一句话在「整批」和
//! 「画字那一刻」两个入口会给出两种译文。

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::dict::mtool::Dict;
use crate::error::Result;

/// 同步查表监听的端口。改了这里必须同步改 `wtool_bridge.js` 的 `LOOKUP_PORT`。
pub const PORT: u16 = 17990;

/// 空闲时多久醒一次去看停止信号。只在没请求的时候生效，不影响应答速度。
const IDLE_MS: u64 = 100;

/// 拿着它就代表查表服务还活着；一 drop，服务线程就收工（和 `task::Handle` 同一招）。
pub struct Guard {
    _stop: Sender<()>,
}

/// 起查表服务。返回的 `Guard` 一 drop，服务线程就退出并放开端口。
///
/// 端口占用会重试一会儿再放弃：上一个任务的服务线程要过 [`IDLE_MS`] 才发现
/// 自己该退了，紧接着起新任务就会撞上「端口被占」——等一小会儿就好，不是真错误。
pub fn spawn(dict: Arc<Dict>) -> Result<Guard> {
    let listener = bind_retry(PORT, Duration::from_secs(2))?;
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    thread::spawn(move || serve(&listener, &dict, &stop_rx));
    Ok(Guard { _stop: stop_tx })
}

fn bind_retry(port: u16, patience: Duration) -> Result<TcpListener> {
    let deadline = Instant::now() + patience;
    loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => return Ok(l),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e.into());
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// 等连接 → 服务这条连接 → 断了再等。一次只服务一条：游戏侧只有一个。
fn serve(listener: &TcpListener, dict: &Dict, stop_rx: &Receiver<()>) {
    // 非阻塞只为「没连接时也能看到停止信号」；一旦握上手就立刻切回阻塞，
    // 应答速度靠的是「阻塞在读上」，不是轮询。
    let _ = listener.set_nonblocking(true);

    loop {
        if stopping(stop_rx) {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                // 只看 [`IDLE_MS`] 一轮就够：有数据立刻返回，超时才轮到看停止信号。
                let _ = stream.set_read_timeout(Some(Duration::from_millis(IDLE_MS)));
                // Nagle 会给小包攒到 40ms 才发，同步通道受不了这个，必须关掉。
                let _ = stream.set_nodelay(true);
                serve_conn(stream, dict, stop_rx);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// 服务一条连接：按行读、逐条查、逐条回。
fn serve_conn(mut stream: TcpStream, dict: &Dict, stop_rx: &Receiver<()>) {
    // 半条行不能丢：读超时是按「这一轮没数据」算的，不是「这条消息完了」。
    let mut rx: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];

    loop {
        match stream.read(&mut buf) {
            // 对端正常关了，回去等重连。
            Ok(0) => return,
            Ok(n) => {
                rx.extend_from_slice(&buf[..n]);
                while let Some(pos) = rx.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = rx.drain(..=pos).collect();
                    let mut body = &line[..line.len() - 1];
                    if body.last() == Some(&b'\r') {
                        body = &body[..body.len() - 1];
                    }
                    let text = String::from_utf8_lossy(body);
                    let Some(reply) = answer(text.trim(), dict) else {
                        continue;
                    };
                    if stream.write_all(reply.as_bytes()).is_err() || stream.write_all(b"\n").is_err()
                    {
                        return;
                    }
                    // 显式 flush：游戏正阻塞等着这一行，不能让它留在缓冲里。
                    if stream.flush().is_err() {
                        return;
                    }
                }
            }
            // 读到超时 = 这一轮没请求。借这个机会看一眼该不该退。
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                if stopping(stop_rx) {
                    return;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// 查一条，组一条回话。认不出的行（不是 `q`、缺字段、JSON 坏了）一律不回 ——
/// 那种行不是这条协议的，回了反而会让游戏侧把上一笔的 id 对错。
fn answer(line: &str, dict: &Dict) -> Option<String> {
    if line.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("t")?.as_str()? != "q" {
        return None;
    }
    let id = v.get("id")?.as_i64()?;
    let src = v.get("text")?.as_str()?;

    let hit = dict.translate(src);

    let mut m = serde_json::Map::new();
    m.insert("t".to_string(), Value::from("a"));
    m.insert("id".to_string(), Value::from(id));
    m.insert("hit".to_string(), Value::from(hit.is_some()));
    if let Some(t) = hit {
        m.insert("text".to_string(), Value::from(t));
    }
    Some(Value::Object(m).to_string())
}

/// 该不该退：收到显式停止，或者 `Guard` 已经没了（`Disconnected`）。
fn stopping(rx: &Receiver<()>) -> bool {
    matches!(rx.try_recv(), Ok(()) | Err(TryRecvError::Disconnected))
}