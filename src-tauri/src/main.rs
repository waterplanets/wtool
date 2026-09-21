// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 两个入口，看命令行参数分流：
//!
//! - 不带参数 → 开窗口，就是平时那个图形界面。
//! - 带 `--game <目录> --dict <文件>` → 不开窗口，直接在终端里跑，
//!   等价于界面上点「开始翻译」，Ctrl+C 相当于点「停止」。
//!   可选 `--font <字体名>`，给翻译时换一个目标字体（缺省不换）。
//! - 带 `--collect --game <目录> --corpus <语料文件>` → 采集模式：不翻只攒料，
//!   启动游戏让玩家正常玩一遍，推上来的原文落进语料文件（见 `--offline-collect`）。
//! - 带 `--offline-collect --game <目录> --out <语料文件>` → 不开游戏，直接扫
//!   `data/*.json` 和 `js/plugins.js` 收语料，打印条数（第 2 步验证用）。
//! - 带 `--translate --corpus <语料文件> --out <词典文件>
//!   [--backend gtx|llm] [--base <接口基址>] [--model <模型>] [--key <ApiKey>] [--batch <N>]` →
//!   把语料喂给翻译后端（`--backend gtx` 免费零配置 <b>缺省</b>；`llm` 走 OpenAI 兼容接口，
//!   缺省 DeepSeek）生成翻译词典。llm 模式 key 也可用环境变量 `WTOOL_LLM_KEY` 传，
//!   两边都给以 `--key` 优先。
//!
//! 命令行模式请用 `cargo run -- ...`（debug）跑：上面那句
//! `windows_subsystem = "windows"` 只在 release 生效，release 下没有终端可看。

use std::path::Path;

use wtool_lib::task::Config;

const USAGE: &str = "用法：wtool --game <游戏目录> --dict <词典文件> [--font <字体名>]
      wtool --collect --game <游戏目录> --corpus <语料文件>
      wtool --offline-collect --game <游戏目录> --out <语料文件>
      wtool --translate --corpus <语料文件> --out <词典文件> [--backend gtx|llm] [--base <接口基址>] [--model <模型>] [--key <ApiKey>] [--batch <N>]
不带参数则打开图形界面。";

/// LLM 接口的预置默认（DeepSeek）。用户不想自定义时，「缺省即可用」。
const DEFAULT_LLM_BASE: &str = "https://api.deepseek.com";
const DEFAULT_LLM_MODEL: &str = "deepseek-chat";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        wtool_lib::run();
        return;
    }

    // 语料/翻译相关的命令单独认，别混进 `parse`（它只管翻译那几个参数）。
    if args.iter().any(|a| a == "--offline-collect") {
        offline_collect(&args);
        return;
    }
    if args.iter().any(|a| a == "--collect") {
        online_collect(&args);
        return;
    }
    if args.iter().any(|a| a == "--translate") {
        translate(&args);
        return;
    }

    wtool_lib::task::run_cli(parse(&args));
}

/// `--translate`：把语料翻成词典。缺参数时尽量用默认值给足，毕竟 GUI 还没有
/// 设置项；只是 key 是硬门槛——没有就得当场报错。
fn translate(args: &[String]) {
    let (Some(corpus), Some(out)) = (flag(args, "--corpus"), flag(args, "--out")) else {
        eprintln!("--translate 需要 --corpus 和 --out。\n{USAGE}");
        std::process::exit(2);
    };

    let base = flag(args, "--base").unwrap_or_else(|| DEFAULT_LLM_BASE.to_string());
    let model = flag(args, "--model").unwrap_or_else(|| DEFAULT_LLM_MODEL.to_string());
    // 后端默认 gtx（免费零配置）；指定 --backend llm 才走 LLM。
    let backend = flag(args, "--backend")
        .map(|b| b.trim().to_string())
        .filter(|b| b == "llm")
        .unwrap_or_else(|| "gtx".to_string());
    let key = flag(args, "--key")
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .or_else(|| std::env::var("WTOOL_LLM_KEY").ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()))
        .unwrap_or_default();
    let batch = flag(args, "--batch")
        .and_then(|b| b.parse::<usize>().ok())
        .unwrap_or(20);

    wtool_lib::task::set_config(Config {
        game_dir: String::new(),
        dict_path: String::new(),
        font: String::new(),
        translate_backend: backend,
        collect: false,
        corpus_path: String::new(),
        out_path: String::new(),
        llm_base_url: base,
        llm_model: model,
        llm_api_key: key,
        llm_batch: batch,
    });

    match wtool_lib::task::translate_cli(&corpus, &out) {
        Ok(msg) => println!("[wtool] {msg}"),
        Err(e) => {
            eprintln!("[wtool] 翻译出错：{e}");
            std::process::exit(2);
        }
    }
}

/// `--offline-collect`：扫游戏目录收语料，打印条数完事，不启动游戏。
fn offline_collect(args: &[String]) {
    let (Some(game), Some(out)) = (flag(args, "--game"), flag(args, "--out")) else {
        eprintln!("--offline-collect 需要 --game 和 --out。\n{USAGE}");
        std::process::exit(2);
    };

    match wtool_lib::task::collect_offline(&game, Path::new(&out)) {
        Ok(msg) => println!("[wtool] {msg}"),
        Err(e) => {
            eprintln!("[wtool] 采集出错：{e}");
            std::process::exit(2);
        }
    }
}

/// `--collect`：在线采集，需要游戏配合（正常玩一遍），阻塞到被 Ctrl+C 杀掉。
fn online_collect(args: &[String]) {
    let (Some(game), Some(corpus)) = (flag(args, "--game"), flag(args, "--corpus")) else {
        eprintln!("--collect 需要 --game 和 --corpus。\n{USAGE}");
        std::process::exit(2);
    };

    wtool_lib::task::run_cli(Config {
        game_dir: game,
        dict_path: String::new(), // 采集模式不查词典，路径置空
        font: String::new(),
        translate_backend: "gtx".to_string(),
        collect: true,
        corpus_path: corpus,
        out_path: String::new(),
        // 采集模式不翻译，后端字段带着默认即可。
        llm_base_url: String::new(),
        llm_model: String::new(),
        llm_api_key: String::new(),
        llm_batch: 20,
    });
}

/// 取 `--名字` 后面那个值。没给就返回 `None`。
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// 认这几个参数。给不齐就带着用法退出 —— 不能悄悄退回去开界面，
/// 那样「少写一个参数」的表现是弹个窗口，太难查了。
fn parse(args: &[String]) -> Config {
    let mut game_dir = None;
    let mut dict_path = None;
    // 目标字体是可选的：不给就空串，翻译时保持游戏原字体。
    let mut font = String::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--game" => game_dir = it.next().cloned(),
            "--dict" => dict_path = it.next().cloned(),
            "--font" => font = it.next().cloned().unwrap_or_default(),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => {
                eprintln!("不认识的参数：{other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    match (game_dir, dict_path) {
        (Some(game_dir), Some(dict_path)) => Config {
            game_dir,
            dict_path,
            font,
            translate_backend: "gtx".to_string(),
            collect: false,
            corpus_path: String::new(),
            out_path: String::new(),
            // 普通翻译循环不触发翻译后端（那是 CLI `--translate` 的事），字段带默认即可。
            llm_base_url: String::new(),
            llm_model: String::new(),
            llm_api_key: String::new(),
            llm_batch: 20,
        },
        _ => {
            eprintln!("--game 和 --dict 都得给。\n{USAGE}");
            std::process::exit(2);
        }
    }
}
