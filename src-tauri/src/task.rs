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
//! 整批那条道有它管不到的地方：开机那批清单里没有的串、自定义菜单在运行时
//! 现拼出来的串。所以另有一条**同步查表**道（见 `engine::lookup`）：游戏侧在
//! 画字那一刻阻塞问一句，这边另一个线程收到就查、查完立刻回，不必先画原文
//! 再重画。两条道共用同一份词典和同一套查法。
//!
//! 引擎怎么说、字节怎么走都在 `engine/` 里，这里只管调度和查表。

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::corpus::{self, online, CorpusFormat};
use crate::dict::{mtool, DictFormat};
use crate::engine::inject::{self, State};
use crate::translate;
use crate::engine::link::{TcpLink, PORT};
use crate::engine::lookup;
use crate::engine::mz::MZ::MzEngine;
use crate::engine::{get_engine_type, resolve_game_dir, Engine, EngineType, Event};
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
    /// 词典文件（`TrsData.json`）的路径。
    ///
    /// 只在翻译模式下必须。采集模式不查词典，空串也放行。
    pub dict_path: String,
    /// 目标字体名（系统中文字体）。空串 = 不替换，只走桥里那层 CJK 缺字兜底。
    pub font: String,
    /// 翻译后端：`gtx`（Google 网页接口，免费零配置）或 `llm`（OpenAI 兼容接口）。
    pub translate_backend: String,
    /// 是不是「采集模式」：不翻只攒料，收到的原文落进 `corpus_path`（第 2 步）。
    pub collect: bool,
    /// 采集模式落盘的语料文件路径。
    pub corpus_path: String,
    /// 离线翻译产出的词典文件路径（`--out`）。
    pub out_path: String,
    /// 语料输出格式（`tsv` / `json`）。只在**新建**语料文件时起作用；
    /// 文件已存在就沿用它自己的格式，别把用户攒了半天的东西换格式重排。
    pub corpus_format: String,
    /// 词典输出格式（`tsv` / `json`）。同上：只在新建输出文件时起作用。
    pub dict_format: String,
    /// LLM 接口基址（OpenAI 兼容），如 `https://api.deepseek.com`。
    pub llm_base_url: String,
    /// LLM 模型名，如 `deepseek-chat`。
    pub llm_model: String,
    /// LLM API key（Bearer）。配置文件里没有时退到环境变量 `WTOOL_LLM_KEY`。
    pub llm_api_key: String,
    /// 一次请求喂多少条原文。
    pub llm_batch: usize,
}

// `String::new()` 和 `Mutex::new()` 都是 const fn，所以这里能直接静态初始化。
static CONFIG: Mutex<Config> = Mutex::new(Config {
    game_dir: String::new(),
    dict_path: String::new(),
    font: String::new(),
    translate_backend: String::new(),
    collect: false,
    corpus_path: String::new(),
    out_path: String::new(),
    corpus_format: String::new(),
    dict_format: String::new(),
    llm_base_url: String::new(),
    llm_model: String::new(),
    llm_api_key: String::new(),
    llm_batch: 20,
});

impl Config {
    /// 从应用配置（`config.json` / 前端表单）拼一份任务配置。
    ///
    /// `collect` 是**任务形态**、不是配置项（同一个游戏目录既能翻也能采），
    /// 所以由调用方给，不写进配置文件。
    pub fn from_app(app: &crate::config::AppConfig, collect: bool) -> Self {
        Self {
            game_dir: app.game_dir.clone(),
            dict_path: app.dict_path.clone(),
            font: app.font.clone(),
            translate_backend: app.translate_backend.clone(),
            collect,
            corpus_path: app.corpus_path.clone(),
            out_path: app.out_path.clone(),
            corpus_format: app.corpus_format.clone(),
            dict_format: app.dict_format.clone(),
            llm_base_url: app.llm_base_url.clone(),
            llm_model: app.llm_model.clone(),
            // key 走 `api_key()`：配置文件里没有就退环境变量。
            llm_api_key: app.api_key(),
            llm_batch: app.llm_batch,
        }
    }
}

/// 写入配置，由 `commands::set_config` 在点「开始翻译」之前调用。
pub fn set_config(cfg: Config) {
    *CONFIG.lock().unwrap() = cfg;
}

/// 取一份配置副本。
///
/// 任务线程起手就抄一份，之后 GUI 再改都不影响已经在跑的这个任务。
/// `commands` 也用它来预判「这次要不要起常驻服务」。
pub fn config() -> Config {
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

    // 正常结束的消息由 `run` 自己说（它才分得清「装完就收工」和「被停止」）；
    // 这里只管兜住错误。
    thread::spawn(move || {
        if let Err(e) = run(&sink, stop_rx) {
            sink.say(format!("翻译结束：{e}"));
        }
    });

    Handle { _stop: stop_tx }
}

/// 在线采集：起后台线程跑 [`run_collecting`]，让玩家正常玩一遍攒语料。
///
/// 语料文件路径从配置读（`--corpus`/界面的「语料文件」）。返回的 `Handle` 一 drop
/// 就停 —— 和 [`trainning`] 同一套停止语义，前端「停止」按钮直接复用。
pub fn trainning_collect(app: &AppHandle) -> Result<Handle> {
    let cfg = config();
    if cfg.game_dir.trim().is_empty() {
        return Err(Error::new("还没填游戏目录"));
    }
    if cfg.corpus_path.trim().is_empty() {
        return Err(Error::new("还没填语料文件路径"));
    }

    let sink = AppSink(app.clone());
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        if let Err(e) = run_collecting(&sink, stop_rx) {
            sink.say(format!("采集结束：{e}"));
        }
    });
    Ok(Handle { _stop: stop_tx })
}

/// 命令行模式：不开窗口，把「开始翻译」那一整套原样跑一遍。
///
/// 存在的意义是自测 —— 图形界面点一遍要动鼠标，命令行一条命令起、
/// 日志直接打在终端上。阻塞到出错或者进程被杀（后者等价于「停止」）。
pub fn run_cli(cfg: Config) {
    set_config(cfg);
    // 这个 Sender 活到函数结束，所以 `recv_timeout` 永远只会超时，不会 Disconnected。
    let (_stop_tx, stop_rx) = mpsc::channel::<()>();
    if let Err(e) = run(&StdoutSink, stop_rx) {
        eprintln!("[wtool] 出错：{e}");
    }
}

/// 无窗口主机（`--host`）：和 [`run_cli`] 跑同一套翻译循环，只是日志多落一份文件。
///
/// 存在的意义是给「游戏目录里那个 `.cmd` / 计划任务」当后台常驻服务：脚本启动
/// 没有终端可看，日志必须落盘，不然出问题什么都查不到。日志写 [`crate::config::log_path`]
/// （`%APPDATA%\wtool\host.log`），每次启动覆盖重写 —— 一次运行一份日志。
pub fn run_host(cfg: Config, log_path: &Path) {
    set_config(cfg);

    // 这个 Sender 活到函数结束，所以 `recv_timeout` 只会超时，不会 Disconnected。
    let (_stop_tx, stop_rx) = mpsc::channel::<()>();

    let sink = match FileSink::open(log_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[wtool] 打不开日志文件 {}：{e}", log_path.display());
            return;
        }
    };
    sink.say(format!("主机启动，日志 {}", log_path.display()));

    if let Err(e) = run(&sink, stop_rx) {
        sink.say(format!("出错：{e}"));
    }
}

/// 线程主体。返回即任务退出（正常停止或者出错）。
fn run(sink: &dyn Reporter, stop_rx: Receiver<()>) -> Result<()> {
    let cfg = config();

    // 采集模式是另一套循环（只攒料不翻），先分出去，别让翻译主循环被 `if` 塞满。
    if cfg.collect {
        return run_collecting(sink, stop_rx);
    }

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

    // 词典是什么格式自己认（MTool 的 JSON / wtool 的行式文本 / wtool 的 JSON）。
    // 认不出来会在这一行报错，日志里说清是哪个文件、支持哪些格式。
    let dict_path = Path::new(dict_path);
    let fmt = DictFormat::sniff(dict_path)?;
    // `Arc` 是为了让同步查表那条道也拿得到同一份。词典整场任务不变
    // （换词典 = 重开任务），所以共享一份只读的就够，不用加锁。
    let dict = Arc::new(mtool::Dict::from_map(fmt.load(dict_path)?));
    sink.say(format!("词典载入 {} 条（{}）", dict.len(), fmt.name()));

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
        // 装：写插件文件 + 在 plugins.js 里打开它（RGSS3 则是转词典 + 追加 Patch.rb）。
        // 幂等，第二次起就只是读一遍确认。
        for line in method.install(Path::new(game_dir), dict_path, &cfg.font)? {
            sink.say(line);
        }

        // 不需要常驻服务的注入方式（RGSS3 的 NWPatch 钩子）：钩子自带词典、
        // 在游戏进程内查表，装完就是终态。这里**不监听端口、不起查表线程**，
        // 直接收工 —— 游戏那边开着就翻译，wtool 开不开都无所谓。
        if !method.needs_server() {
            sink.say("钩子已安装（持久）。启动游戏即可翻译，无需保持 wtool 打开。".to_string());
            return Ok(());
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

    // 同步查表另开一条通道。起不来不算失败：游戏侧会自己退回「先画原文、
    // 等整批回话到了再重画」的老路，所以这里只提醒一声，任务继续。
    // 这个名字前面有下划线，但它得活到 `run` 结束 —— 它一 drop，
    // 查表线程就收工、端口才放开，否则下一轮任务会撞上「端口被占」。
    let _lookup = match lookup::spawn(Arc::clone(&dict)) {
        Ok(guard) => {
            sink.say(format!("同步查表已就绪 127.0.0.1:{}", lookup::PORT));
            Some(guard)
        }
        Err(e) => {
            sink.say(format!("同步查表通道起不来（{e}），本轮退回异步重画"));
            None
        }
    };

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
                // 目标字体跟任务走：连上时下发一次。空串表示不换，游戏保持原字体，
                // 只靠桥里那层 CJK 兜底补缺字。
                if !cfg.font.is_empty() {
                    engine.set_font(&cfg.font)?;
                    sink.say(format!("目标字体：{}", cfg.font));
                }
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

/// 扫描游戏目录、离线收一份语料并写盘，返回给人看的话。
///
/// 不启动游戏、不注入 —— 纯粹读游戏目录里的数据文件，出条数。
/// 命令行 `--offline-collect` 和界面的采集页「离线扫描」都走这条。
///
/// 别当它「很快」：RGSS3 的数据打包在 `Game.rgss3a` 里（本机那个 975MB），
/// 要先读归档再走 Ruby Marshal，慢是正常的。但**不整包解密** —— 只按文件表
/// 定位要的那几个 `Data/*.rvdata2`，逐个 seek 读。这里是同步函数，不过 Tauri
/// 命令跑在 worker 线程上，界面不会冻；真嫌慢再挪进后台线程（那时复用
/// `trainning_collect` 的 `Handle` 语义）。
pub fn collect_offline(game_dir: &str, out: &std::path::Path, fmt: CorpusFormat) -> Result<String> {
    let dir = resolve_game_dir(game_dir);

    // 「去哪儿找文本」按**引擎的数据格式**分派，不按游戏分：MV/MZ 是明文 JSON、
    // 按字段名取，RGSS3 是 Ruby Marshal、还得先解归档。两边只共用 Corpus、
    // 粒度、过滤和落盘格式，取文本这一段没得共用。
    //
    // 分派写在这里（调用方）而不写进 corpus，是为了不让 corpus 反向依赖
    // engine —— 那会转成环。顺序也定死：先 resolve 再判类型，否则用户把
    // 路径填到 `Game.exe` 上时判不出来。
    let corpus = match get_engine_type(&dir) {
        EngineType::Mz | EngineType::Mv => corpus::offline::scan_json(&dir)?,
        EngineType::Rgss3 => corpus::rgss3::scan(&dir)?,
        EngineType::Unknown => {
            return Err(Error::new(format!(
                "{} 里既没有 data/ 目录，也没有 Game.rgss3a / Data/*.rvdata2，\
                 游戏目录是不是填错了？",
                dir.display()
            )))
        }
    };

    let n = corpus::save(out, &corpus, fmt)?;
    Ok(format!(
        "{} 扫描完毕，{}，已写进 {}（{}，去重 {n} 条）",
        dir.display(),
        corpus.stats(),
        out.display(),
        fmt.name()
    ))
}

/// 命令行翻译入口：读语料 + 目标词典（断点续传集）→ 调翻译后端 → 写回目标词表。
///
/// `out` 是 wtool 词典文件：这次翻的新译文合并**已存在**的译文一起写回同一个
/// 文件，所以这文件可以安全地跨多次运行累积——每次只补翻缺的那部分（`resume`）。
/// 后端在配置里选：`translate_backend == "gtx"` 走免费谷歌接口（零配置），
/// 其余值（如 `"llm"`）走 OpenAI 兼容接口（要 base/model/key）。
pub fn translate_cli(corpus_path: &str, out_path: &str) -> Result<String> {
    // 命令行靠 Ctrl+C 停，不需要停止通道；给一个永远不会断的接收端。
    let (_stop_tx, stop_rx) = mpsc::channel::<()>();
    run_translate_file(&StdoutSink, corpus_path, out_path, &stop_rx)
}

/// 离线翻译入口：起一个后台线程跑 [`run_translate_file`]，进度走 `trans-status`。
///
/// 返回的 `Handle` 进 `commands::CURRENT` —— 和直播翻译、在线采集**同一套停止
/// 语义**：界面上的「停止」对三个任务是一个意思。停止在**批与批之间**生效
/// （见 `run_translate_file` 里的 flush），本轮已翻的译文已经落盘，下次接着跑。
pub fn trainning_translate(app: &AppHandle) -> Result<Handle> {
    let cfg = config();
    let corpus_path = cfg.corpus_path.trim().to_string();
    let out_path = cfg.out_path.trim().to_string();
    if corpus_path.is_empty() {
        return Err(Error::new("还没填语料文件路径（--corpus）"));
    }
    if out_path.is_empty() {
        return Err(Error::new("还没填输出词典路径"));
    }

    let sink = AppSink(app.clone());
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        sink.say("开始离线翻译…".to_string());
        let result = run_translate_file(&sink, &corpus_path, &out_path, &stop_rx);
        // 「已停止」不是错，照实说就行，别吓人。
        match result {
            Ok(msg) => sink.say(msg),
            Err(e) => sink.say(format!("离线翻译结束：{e}")),
        }
    });
    Ok(Handle { _stop: stop_tx })
}

/// 真正的翻译主体：把语料翻成词典。CLI 与 GUI 共用一条道，`sink` 决定进度往哪说。
///
/// `out` 是 wtool 词典文件：这次翻的新译文合并**已存在**的译文一起写回同一个
/// 文件，所以这文件可以安全地跨多次运行累积——每次只补翻缺的那部分（`resume`）。
/// 后端在配置里选：`translate_backend == "gtx"` 走免费谷歌接口（零配置），
/// 其余值（如 `"llm"`）走 OpenAI 兼容接口（要 base/model/key）。
///
/// `stop` 是「停止」通道：只**在批与批之间**看一眼，不停正在飞的那批请求 ——
/// 那一批的译文已经在写了，硬切只会把文件写坏。所以「停止」的含义是
/// 「本轮到此为止」：已翻的已落盘，下次接着跑。
fn run_translate_file(
    sink: &dyn Reporter,
    corpus_path: &str,
    out_path: &str,
    stop: &Receiver<()>,
) -> Result<String> {
    let cfg = config();

    // 翻译后端二选一：gtx 免费零配置；llm 要 base/model/key 三样都齐了才行。
    let use_gtx = cfg.translate_backend.trim() == "gtx";
    if !use_gtx && cfg.llm_api_key.trim().is_empty() {
        return Err(Error::new("没给 API key：请用 --key 传，或设环境变量 WTOOL_LLM_KEY"));
    }
    if !use_gtx && cfg.llm_base_url.trim().is_empty() {
        return Err(Error::new("没给接口基址（--base）"));
    }
    if !use_gtx && cfg.llm_model.trim().is_empty() {
        return Err(Error::new("没给模型名（--model）"));
    }

    let corpus = corpus::load(Path::new(corpus_path))?;
    let out = Path::new(out_path);

    // 输出格式：文件已存在就沿用它自己的（嗅探得出），不存在才按配置新建 ——
    // 这样「续传」一定接得上原来的文件，而「另存为换个格式」是 `convert_dict` 的事。
    //
    // 开工前就认格式，别等翻了半天才炸：
    //  - 已存在但不是 wtool 能读写的格式（含 MTool 的只读 JSON）→ 现在就说清楚，
    //    省得一批都翻完了才报「不是 UTF-8」，白烧 API 又让用户以为坏了一半。
    //  - 已存在且是 wtool 自身格式 → 当断点续传集，继续翻缺的那部分。
    //  - 不存在 → 首次运行，按配置的格式新建。
    let out_fmt = if out.exists() {
        match DictFormat::sniff(out)? {
            DictFormat::MtoolJson => {
                return Err(Error::new(format!(
                    "{} 是 MTool 词典（只读格式），wtool 不会往里写。\n\
                     请给 wtool 自己的输出路径，例如一个新文件 `dict.tsv` 或 `dict.json`。",
                    out.display()
                )));
            }
            fmt => fmt, // wtool 自身格式，可续传。
        }
    } else {
        DictFormat::parse_out(&cfg.dict_format)
    };

    // 已有译文 = 断点续传集：这文件里翻过的原文，这次不再翻。
    let merged_init: std::collections::BTreeMap<String, String> = if out.exists() {
        out_fmt.load(out)?
    } else {
        std::collections::BTreeMap::new()
    };
    let resume_has = |src: &str| merged_init.contains_key(src);

    // 按配置挑后端。gtx 白嫖零配置；llm 把 base/model/key 三件套带进实现。
    let backend: Box<dyn translate::TranslateBackend> = if use_gtx {
        Box::new(translate::gtx::Backend::new())
    } else {
        Box::new(translate::openai::Backend {
            base_url: cfg.llm_base_url.clone(),
            model: cfg.llm_model.clone(),
            api_key: cfg.llm_api_key.clone(),
        })
    };

    let mut report = |msg: &str| sink.say(msg.to_string());

    // 每批把「盘上已有的 ∪ 新翻的」整体写回 out。死在批与批之间，前面的译文
    // 已落盘，重跑时被 resume 跳过 —— 断点续传就接在这。每次从盘上重读而不是
    // 捕获可变状态，是为了不和上面的 resume 借冲突。
    let mut flush = |dict: &std::collections::BTreeMap<String, String>| {
        // 批与批之间看一眼「停止」：`Ok(())` 是显式叫停，`Disconnected` 是
        // 拿着 Handle 的那位没了（任务被顶掉）—— 两种都该收工。
        match stop.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                return Err(Error::new("已停止（本轮已翻的译文已落盘，下次接着跑）"))
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let mut snapshot: std::collections::BTreeMap<String, String> =
            if out.exists() { out_fmt.load(out)? } else { Default::default() };
        for (k, v) in dict {
            if !v.is_empty() {
                snapshot.insert(k.clone(), v.clone());
            }
        }
        out_fmt
            .write(out, &snapshot)
            .map_err(|e| Error::new(format!("写翻译文件失败：{e}")))?;
        Ok::<(), Error>(())
    };

    let dict = translate::translate(
        &corpus,
        backend.as_ref(),
        cfg.llm_batch.max(1),
        &resume_has,
        &mut report,
        &mut flush,
    )?;

    // 累计条数 = 盘上现在的（本次已写进去了）。
    let total = if out.exists() {
        out_fmt.load(out)?.len()
    } else {
        dict.len()
    };
    Ok(format!(
        "本轮翻了 {} 条，词典累计 {} 条（{}），已写进 {}",
        dict.len(),
        total,
        out_fmt.name(),
        out.display()
    ))
}

/// 采集模式的主循环：跟翻译 loop 同构（注入 → 监听 → poll），差别只在收到
/// `Text` / `Terms` 时往语料里收，**不回写译文**（`apply_terms` 回空表）。
///
/// 让玩家正常玩一遍，动态料（脚本拼的消息、`\v[n]` 展开后的真话）就落进语料了。
fn run_collecting(sink: &dyn Reporter, stop_rx: Receiver<()>) -> Result<()> {
    let cfg = config();
    let game_dir = cfg.game_dir.trim();
    let corpus_path = cfg.corpus_path.trim();
    let dict_path = cfg.dict_path.trim();

    if game_dir.is_empty() {
        return Err(Error::new("还没填游戏目录"));
    }
    if corpus_path.is_empty() {
        return Err(Error::new("还没填语料文件路径（--corpus）"));
    }

    let kind = get_engine_type(Path::new(game_dir));
    sink.say(format!("检测到引擎：{}", kind.name()));
    if kind == EngineType::Unknown {
        return Err(Error::new("这个目录既不像 MZ 也不像 MV，游戏目录是不是填错了？"));
    }

    let mut collector = online::Collector::open(
        Path::new(corpus_path).to_path_buf(),
        CorpusFormat::parse(&cfg.corpus_format),
    )?;
    sink.say(format!("已有语料 {}", collector.stats()));

    // 注入方式和翻译模式同一套：桥照常把原文推上来，只是 wtool 不回译文。
    let methods = inject::mechanisms(kind);
    let mut injected = false;
    for method in &methods {
        let report = method.probe(Path::new(game_dir));
        if report.state == State::NotApplicable {
            sink.say(format!("{}：{}", method.name(), report.detail));
            continue;
        }
        sink.say(format!("注入方式：{}（{}）", method.name(), report.detail));
        for line in method.install(Path::new(game_dir), Path::new(dict_path), &cfg.font)? {
            sink.say(line);
        }
        // 在线采集靠游戏侧把文本推上来；不需要常驻服务的注入方式（RGSS3）
        // 从不回报文本，这条路对它不成立 —— 当场说清楚，别让用户对着
        // 「正在监听」干等。RGSS3 收料请用「离线扫描」。
        if !method.needs_server() {
            return Err(Error::new(
                "这个引擎的钩子不向 wtool 回报文本，在线采集不适用。请改用「离线扫描」收语料。",
            ));
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
    sink.say(format!("正在监听 127.0.0.1:{PORT}，请启动游戏并正常游玩（采集模式）"));
    let mut engine = MzEngine::new(link);
    let mut last_save = Instant::now();

    loop {
        match stop_rx.recv_timeout(TICK) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let n = collector.save()?;
                sink.say(format!("采集结束，{}，写进 {n} 条", collector.stats()));
                return Ok(());
            }
        }

        let Some(ev) = engine.poll()? else {
            continue;
        };
        match ev {
            Event::Attached => {
                sink.say("游戏已连上（采集模式）".to_string());
                // 开局可能已经显示着对话框，补一次当前原文。
                engine.refresh()?;
            }
            Event::Text(d) => {
                collector.on_text(&d.text);
                sink.say(format!("已收段落 #{}：{}", d.seq, brief(&d.text)));
            }
            Event::Terms { label, loose, texts } => {
                collector.on_terms(&label, loose, &texts);
                // 回空表：游戏侧替换 0 条，画面保持原文 —— 这就是「只攒料不翻」。
                engine.apply_terms(&[])?;
                sink.say(format!("{label} 收到 {} 条原文", texts.len()));
            }
            Event::Closed { .. } => {}
        }

        // 定时落盘：玩到一半被强杀，也不至于丢掉已经玩到的料。
        if last_save.elapsed() > Duration::from_secs(2) {
            let _ = collector.save();
            last_save = Instant::now();
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

/// 主机模式：终端 + 日志文件各一份。
///
/// 每条都 flush，是故意的：主机多半是被强杀（关终端、关机）停下的，
/// 攒在缓冲区里那几行日志恰恰是最关键的那几行。
struct FileSink {
    file: Mutex<std::fs::File>,
}

impl FileSink {
    fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 覆盖重写：一次运行一份日志，不跟上次的混在一起。
        let file = std::fs::File::create(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl Reporter for FileSink {
    fn say(&self, msg: String) {
        println!("[wtool] {msg}");
        use std::io::Write;
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{msg}");
            let _ = f.flush();
        }
    }
}
