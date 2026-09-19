//! 与游戏进程之间的字节通道。
//!
//! 线格式：一行一条 JSON，UTF-8，`\n` 分隔。
//!
//! 为什么用这个而不是 WebSocket：nwjs 的窗口里有完整的 Node 集成
//! （游戏自带的 `UskDebugDevToolsOnStart.js` 里直接 `require('nw.gui')` 就是实锤），
//! 所以游戏内 JS `require('net')` 就能连回来，游戏侧一行依赖都不用加，
//! 我们这侧也只用 `std::net`，不新增任何 crate。CDP 那条路以后仍要
//! 走同一个应用层协议，这份代码不浪费。

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};

use crate::error::{Error, Result};

/// wtool 监听的固定端口。改了这里必须同步改 `wtool_bridge.js` 的 `PORT`。
pub const PORT: u16 = 17989;

/// 传输层接口。
pub trait Link: Send {
    /// 发一条消息。参数里不要带换行，换行由实现补。
    fn send(&mut self, json: &str) -> Result<()>;
    /// 非阻塞地收一条消息。`Ok(None)` = 此刻没有。
    fn poll(&mut self) -> Result<Option<String>>;
}

/// 监听本机 TCP，等游戏连进来。
/// 同一时刻只维持一条连接：这个场景就是 1 写 1 读，
/// 游戏重连时新连接直接顶掉旧的。
pub struct TcpLink {
    listener: TcpListener,
    conn: Option<TcpStream>,
    /// 还没凑够一整行的字节。
    rx: Vec<u8>,
    /// 已经切好的整行，等 `poll` 来取。
    lines: VecDeque<String>,
}

impl TcpLink {
    /// 绑定端口并开始监听。非阻塞。
    /// 端口被占（多半是上一个 wtool 进程没退干净）会直接报错，
    /// 不静默换端口——换了游戏侧就找不到了。
    pub fn bind(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            conn: None,
            rx: Vec::new(),
            lines: VecDeque::new(),
        })
    }

    /// 游戏当前是否连着。
    pub fn is_attached(&self) -> bool {
        self.conn.is_some()
    }

    /// 有新连接就顶掉旧的。返回是否发生了更替。
    ///
    /// 不做「同一个游戏重复连」的判重：游戏侧断线重连是新连接，
    /// 旧的一定已经死了，判重反而容易把活连接误判成死的。
    fn accept(&mut self) -> Result<bool> {
        match self.listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(true)?;
                stream.set_nodelay(true)?;
                self.conn = Some(stream);
                // 旧连接留下的半条数据和已经没人要的整行，一起丢掉。
                self.rx.clear();
                self.lines.clear();
                Ok(true)
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// 把套接字里现有的字节读干净，切成整行推进 `lines`。
    fn drain(&mut self) -> Result<()> {
        let mut closed = false;
        let mut chunk: Vec<u8> = Vec::new();

        if let Some(conn) = self.conn.as_mut() {
            let mut buf = [0u8; 4096];
            loop {
                match conn.read(&mut buf) {
                    // read 返回 0 = 对端正常关闭。
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => chunk.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    // 对端被硬关掉（游戏崩了、被任务管理器杀了、进程直接 exit）
                    // 时 read 报的是 ConnectionReset，而不是老老实实返回 0。
                    // 语义一样是「这条连接没了」，当断开处理、等它重连就行 ——
                    // 不能让它冒上去把整个翻译任务带走。
                    Err(e)
                        if e.kind() == ErrorKind::ConnectionReset
                            || e.kind() == ErrorKind::ConnectionAborted =>
                    {
                        closed = true;
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }

        if closed {
            self.conn = None;
            self.rx.clear();
        }

        if chunk.is_empty() {
            return Ok(());
        }
        self.rx.extend_from_slice(&chunk);

        while let Some(pos) = self.rx.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.rx.drain(..=pos).collect();
            // 去掉行尾的 \n，再吃掉 \r，免得 Windows 侧写入时对不上。
            let mut body = &line[..line.len() - 1];
            if body.last() == Some(&b'\r') {
                body = &body[..body.len() - 1];
            }
            let text = String::from_utf8_lossy(body).trim().to_string();
            if !text.is_empty() {
                self.lines.push_back(text);
            }
        }
        Ok(())
    }
}

impl Link for TcpLink {
    fn send(&mut self, json: &str) -> Result<()> {
        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| Error::new("游戏还没连上来，写不进去"))?;

        // 套接字平时是非阻塞的（`poll` 靠它），但 `write_all` 一旦写满发送
        // 缓冲区、半路撞上 WouldBlock 就会直接报错。之前几段对话的回写只有
        // 几十字节，从没写满过，所以一直没暴露；菜单整表、物品/技能的名字与
        // 说明这几批动辄几百 KB，必然写满。写的时候临时切回阻塞、写完切回来，
        // 收的那一侧不受影响（它不是同一时刻在跑的）。
        conn.set_nonblocking(false)?;
        let written = write_message(conn, json);
        conn.set_nonblocking(true)?;
        written
    }

    fn poll(&mut self) -> Result<Option<String>> {
        // 先把上一轮切好的整行交出去，避免积压。
        if let Some(line) = self.lines.pop_front() {
            return Ok(Some(line));
        }
        self.accept()?;
        self.drain()?;
        Ok(self.lines.pop_front())
    }
}

/// 一条消息 = 正文 + 一个 `\n` 结束符。分成两步写是为了不把正文和换行
/// 拼成一个临时字符串，几百 KB 的正文再复制一份没必要。
fn write_message(conn: &mut TcpStream, json: &str) -> Result<()> {
    conn.write_all(json.as_bytes())?;
    conn.write_all(b"\n")?;
    conn.flush()?;
    Ok(())
}
