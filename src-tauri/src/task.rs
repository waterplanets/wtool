//! 业务层：一个翻译任务 = 一个线程 + 一条「停止」通道。
//!
//! 线程里干的活：
//!
//! ```text
//!   读全局配置（GUI 上填的）
//!     → get_engine_type() 认出是什么引擎
//!     → 载入词典索引
//!     → 监听端口，等游戏里的 JS 连上来
//!     → 循环：poll 出一段对话 → 查词典 → apply 回写译文
//! ```
//!
//! 菜单文字（道具/技能/存档…）另走一条道：它渲染时是同步取字的，等不了一趟
//! 往返，所以游戏侧开机把 `System.json` 的 `terms` 整张表问过来（`Event::Terms`），
//! 这里查完一次性回过去，游戏侧直接在内存里改掉。物品/技能的名字与说明
//! （`$dataItems` / `$dataSkills` / …）走的是同一条道，只是查法要退级。
//!
//! 引擎怎么说、字节怎么走都在 `engine/` 里，这里只管调度和查表。

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::dict::mtool;
use crate::engine::inject::{self, State};
use crate::engine::link::{TcpLink, PORT};
use crate::engine::mz::MZ::MzEngine;
use crate::engine::{get_engine_type, Engine, EngineType, Event};
use crate::error::{Error, Result};

/// 往前端推状态的通道名。前端 `listen("trans-status", ...)`。
const EVENT_STATUS: &str = "trans-status";

/// 一轮循环的间隔。同时也是「按下停止」到「线程真的退出」的延迟上限。
const TICK: Duration = Duration::from_millis(100);

/// GUI 上填的那几个变量，全工程就这一份。
///
/// 对照 C：相当于 `static struct config g_config;`，只是数据装在锁里面，
/// 而不是「一把锁 + 一堆散落的全局变量」。
#[derive(Debug, Clone)]
pub struct Config {
    /// 游戏根目录，也就是 `Game.exe` 所在的那个目录。
    pub game_dir: String,
    /// 词典文件（`TrsData.bin`）的路径。
    pub dict_path: String,
}

// `String::new()` 和 `Mutex::new()` 都是 const fn，所以这里能直接静态初始化。
static CONFIG: Mutex<Config> = Mutex::new(Config {
    game_dir: String::new(),
    dict_path: String::new(),
});

/// 写入配置，由 `commands::set_config` 在点「开始翻译」之前调用。
pub fn set_config(cfg: Config) {
    *CONFIG.lock().unwrap() = cfg;
}

/// 取一份配置副本。
///
/// 任务线程起手就抄一份，之后 GUI 再改都不影响已经在跑的这个任务。
fn config() -> Config {
    CONFIG.lock().unwrap().clone()
}

pub struct Handle {
    /// 这个字段没人读，它的存在本身就是意义：`Handle` 被 drop 时 `Sender`
    /// 跟着 drop，线程那边的 `recv_timeout` 立刻返回 `Disconnected`，任务退出。
    /// 换句话说是「谁拿着 Handle 谁就能叫停这个任务」。
    _stop: mpsc::Sender<()>,
}

/// 起一个翻译任务。返回的 `Handle` 一 drop，任务就退出。
pub fn trainning(app: &AppHandle) -> Handle {
    let sink = AppSink(app.clone());
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    thread::spawn(move || match run(&sink, stop_rx) {
        Ok(()) => sink.say("翻译结束".to_string()),
        Err(e) => sink.say(format!("翻译结束：{e}")),
    });

    Handle { _stop: stop_tx }
}

/// 命令行模式：不开窗口，把「开始翻译」那一整套原样跑一遍。
///
/// 存在的意义是自测 —— 图形界面点一遍要动鼠标，命令行一条命令起、
/// 日志直接打在终端上。阻塞到出错或者进程被杀（后者等价于「停止」）。
pub fn run_cli(cfg: Config) {
    set_config(cfg);

    // 这个 Sender 活到函数结束，所以 `recv_timeout` 永远只会超时，不会 Disconnected。
    let (_stop_tx, stop_rx) = mpsc::channel::<()>();

    match run(&StdoutSink, stop_rx) {
        Ok(()) => println!("[wtool] 翻译结束"),
        Err(e) => eprintln!("[wtool] 翻译出错：{e}"),
    }
}

/// 线程主体。返回即任务退出（正常停止或者出错）。
fn run(sink: &dyn Reporter, stop_rx: Receiver<()>) -> Result<()> {
    let cfg = config();
    let game_dir = cfg.game_dir.trim();
    let dict_path = cfg.dict_path.trim();

    if game_dir.is_empty() {
        return Err(Error::new("还没填游戏目录"));
    }
    if dict_path.is_empty() {
        return Err(Error::new("还没填词典路径"));
    }

    // 「自动检测引擎类型」：只要填了游戏目录，用什么引擎是猜出来的，不用选。
    let kind = get_engine_type(Path::new(game_dir));
    sink.say(format!("检测到引擎：{}", kind.name()));
    if kind == EngineType::Unknown {
        return Err(Error::new(
            "这个目录既不像 MZ 也不像 MV，游戏目录是不是填错了？",
        ));
    }

    let dict = mtool::Dict::load(Path::new(dict_path))?;
    sink.say(format!("词典载入 {} 条", dict.len()));

    // 挑一条注入方式把通道架进去。
    //
    // 「一种引擎可能有多种注入方式，而且不是每种都通用」这件事，全部收在
    // `engine::inject` 里：这里只按优先级问一遍，挑到第一条能用的就用它。
    // 挑不到必须当场报错，不能让用户对着「正在监听」的日志干等 ——
    // 通道没架进去，游戏永远不会连过来。
    let methods = inject::mechanisms(kind);
    let mut injected = false;
    for method in &methods {
        let report = method.probe(Path::new(game_dir));
        if report.state == State::NotApplicable {
            // 这条路在这个游戏上不成立，说一声再试下一条。
            sink.say(format!("{}：{}", method.name(), report.detail));
            continue;
        }

        sink.say(format!("注入方式：{}（{}）", method.name(), report.detail));
        // 装：写插件文件 + 在 plugins.js 里打开它。幂等，第二次起就只是读一遍确认。
        for line in method.install(Path::new(game_dir))? {
            sink.say(line);
        }
        injected = true;
        break;
    }
    if !injected {
        return Err(Error::new(format!(
            "{}（{}）上还没有可用的注入方式",
            kind.name(),
            kind.runtime().name()
        )));
    }

    let link = bind_with_retry(PORT, Duration::from_secs(1))?;
    sink.say(format!("正在监听 127.0.0.1:{PORT}，请启动游戏"));

    // MV 与 MZ 的消息系统同名（`Window_Message.prototype.startMessage`、
    // `$gameMessage._texts`），所以这一份实现两个都能用，不按 kind 分叉。
    let mut engine = MzEngine::new(link);
    let mut hit = 0u64;
    let mut miss = 0u64;

    loop {
        // 停止信号和「睡一会儿」合成一次调用：要么超时（继续干活），
        // 要么收到信号 / 对端关闭（收工）。
        match stop_rx.recv_timeout(TICK) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }

        let Some(ev) = engine.poll()? else {
            continue;
        };

        match ev {
            Event::Attached => {
                sink.say("游戏已连上".to_string());
                // 万一这会儿游戏已经显示着对话框，补问一次当前内容。
                engine.refresh()?;
            }

            Event::Text(d) => match dict.translate(&d.text) {
                Some(target) => {
                    engine.apply(&target)?;
                    hit += 1;
                    sink.say(format!(
                        "#{} [{}] {} → {}",
                        d.seq,
                        stat(hit, miss),
                        brief(&d.text),
                        brief(&target)
                    ));
                }
                None => {
                    miss += 1;
                    sink.say(format!(
                        "#{} [{}] 词典里没有：{}",
                        d.seq,
                        stat(hit, miss),
                        brief(&d.text)
                    ));
                }
            },

            // 对话关了不用做什么：下一段 `Text` 里带着新的 seq 和原文，
            // 写回去时自然对得上。
            Event::Closed { .. } => {}

            // 整批文本：菜单词条、物品/技能的名字与说明、事件文本、独特数据。
            // 开机各问一次就完事，之后不会再来。
            //
            // 查法按 `loose` 分两种：
            //   - 菜单词条（false）走精确查 —— 词条本身就是词典的 key 粒度，
            //     整条查不中就是没有；退到片段反而会把 `ーーーー` 这类
            //     分隔符翻出鬼来。
            //   - 其余三批（true）走 `translate` 的三级退级 —— 说明里混着
            //     `\n` 和控制码，词典里既有整段的 key，也有只盖住半句的片段
            //     key，不退级就翻不全。
            Event::Terms {
                loose,
                label,
                texts,
            } => {
                let mut map = Vec::new();
                for t in &texts {
                    let hit = if loose {
                        dict.translate(t)
                    } else {
                        dict.exact(t).map(str::to_string)
                    };
                    if let Some(target) = hit {
                        map.push((t.clone(), target));
                    }
                }
                engine.apply_terms(&map)?;

                sink.say(format!("{label} {} 条，翻上 {} 条", texts.len(), map.len()));
            }
        }
    }
}

/// 绑定端口，短时间重试。
///
/// 场景：GUI 上连点两次「开始翻译」。旧任务的监听套接字要等它那一轮
/// `recv_timeout` 超时（最多 `TICK`）才会关，新任务这会儿 bind 会撞上
/// 「端口被占」——等一小会儿就好，不是真错误。
fn bind_with_retry(port: u16, patience: Duration) -> Result<TcpLink> {
    let deadline = Instant::now() + patience;
    loop {
        match TcpLink::bind(port) {
            Ok(link) => return Ok(link),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// 命中率计数，日志里一行带过。
fn stat(hit: u64, miss: u64) -> String {
    format!("{hit} 命中 / {miss} 未命中")
}

/// 把一段原文压成单行短串，免得一条日志刷满屏。
///
/// 换行显式写成 `\n`：词典的 key 就是这种用 `\n` 连起来的整段，
/// 日志里看得见换行位置，才知道 key 是不是对得上。
fn brief(s: &str) -> String {
    let one = s.replace('\n', "\\n");
    let mut it = one.chars();
    let head: String = it.by_ref().take(120).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 状态消息的出口。
///
/// 对照 C：相当于把 `void (*report)(const char *)` 这根函数指针存下来，
/// 同一个 `run()` 谁调、消息就流到谁那儿 —— 图形界面调就推给前端，
/// 命令行调就打在终端。`run()` 自己不需要知道自己在哪种模式下跑。
pub trait Reporter: Send + Sync {
    fn say(&self, msg: String);
}

/// 图形界面模式：终端留一份，同时 `emit` 给前端。
struct AppSink(AppHandle);

impl Reporter for AppSink {
    fn say(&self, msg: String) {
        println!("[wtool] {msg}");
        let _ = self.0.emit(EVENT_STATUS, msg);
    }
}

/// 命令行模式：只打终端。
struct StdoutSink;

impl Reporter for StdoutSink {
    fn say(&self, msg: String) {
        println!("[wtool] {msg}");
    }
}
