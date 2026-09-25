// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 两个入口，看命令行参数分流：
//!
//! - 不带参数 → 开窗口，就是平时那个图形界面。
//! - 带 `--game <目录> --dict <文件>` → 不开窗口，直接在终端里跑，
//!   等价于界面上点「开始翻译」，Ctrl+C 相当于点「停止」。
//! - 带 `--host --game <目录>` → 同样的翻译循环，但日志落盘
//!   （`%APPDATA%\wtool\host.log`）。给游戏目录里那个 `wtool-host.cmd`
//!   当后台服务用：关掉终端也留着日志。
//! - 带 `--collect --game <目录> --corpus <语料文件>` → 采集模式：不翻只攒料。
//! - 带 `--offline-collect --game <目录> --out <语料文件> [--format tsv|json]` →
//!   不开游戏，直接扫游戏目录收语料，打印条数。
//! - 带 `--translate --corpus <语料文件> --out <词典文件> [--backend gtx|llm]
//!   [--base <接口基址>] [--model <模型>] [--key <ApiKey>] [--batch <N>]
//!   [--format tsv|json]` → 把语料喂给翻译后端生成词典。
//!
//! **所有参数都可以不写**：先读 `%APPDATA%\wtool\config.json`（界面上存的那份），
//! 命令行再逐项覆盖。所以「界面配好一次，之后脚本一条命令起」是成立的 ——
//! 也是 `--host` 能只写 `--game` 的原因。
//!
//! 命令行模式请用 `cargo run -- ...`（debug）跑：上面那句
//! `windows_subsystem = "windows"` 只在 release 生效，release 下没有终端可看。

use std::path::Path;

use wtool_lib::config::AppConfig;
use wtool_lib::corpus::CorpusFormat;
use wtool_lib::task::{self, Config};

const USAGE: &str = "用法：wtool [命令] [参数…]
  （不带参数则打开图形界面；下面所有参数都可用界面里存的配置代替）

  wtool --game <游戏目录> --dict <词典文件> [--font <字体名>]
  wtool --host --game <游戏目录>                 常驻主机，日志落盘
  wtool --collect --game <游戏目录> --corpus <语料文件>
  wtool --offline-collect --game <游戏目录> --out <语料文件> [--format tsv|json]
  wtool --translate --corpus <语料文件> --out <词典文件> \\
        [--backend gtx|llm] [--base <接口基址>] [--model <模型>] [--key <ApiKey>] \\
        [--batch <N>] [--format tsv|json]

配置与 API Key 读 %APPDATA%\\wtool\\config.json（在 wtool 界面里设置）。";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }

    if args.is_empty() {
        wtool_lib::run();
        return;
    }

    // 各命令分开认。认不出一个已知命令、又没有任何已知参数时，报用法退出 ——
    // 不能悄悄退回去开界面，那样「少写一个参数」的表现是弹个窗口，太难查了。
    if has(&args, "--offline-collect") {
        offline_collect(&args);
    } else if has(&args, "--collect") {
        online_collect(&args);
    } else if has(&args, "--translate") {
        translate(&args);
    } else if has(&args, "--host") {
        host(&args);
    } else if has(&args, "--game") || has(&args, "--dict") {
        plain(&args);
    } else {
        eprintln!("不认识的参数。\n{USAGE}");
        std::process::exit(2);
    }
}

/// `--host`：常驻主机，日志落盘。给游戏目录里那个 `.cmd` 当后台服务。
fn host(args: &[String]) {
    let cfg = base_config(args);
    let Some(log) = wtool_lib::config::log_path() else {
        eprintln!("找不到应用数据目录（APPDATA 未设置），没法落日志。");
        std::process::exit(2);
    };
    task::run_host(cfg, &log);
}

/// `--offline-collect`：扫游戏目录收语料，打印条数完事，不启动游戏。
fn offline_collect(args: &[String]) {
    let cfg = base_config(args);
    if cfg.game_dir.is_empty() || cfg.out_path.is_empty() {
        eprintln!("--offline-collect 需要 --game 和 --out。\n{USAGE}");
        std::process::exit(2);
    }
    let fmt = CorpusFormat::parse(&cfg.corpus_format);
    match task::collect_offline(&cfg.game_dir, Path::new(&cfg.out_path), fmt) {
        Ok(msg) => println!("[wtool] {msg}"),
        Err(e) => {
            eprintln!("[wtool] 采集出错：{e}");
            std::process::exit(2);
        }
    }
}

/// `--collect`：在线采集，需要游戏配合（正常玩一遍），阻塞到被 Ctrl+C 杀掉。
fn online_collect(args: &[String]) {
    let mut cfg = base_config(args);
    if cfg.game_dir.is_empty() || cfg.corpus_path.is_empty() {
        eprintln!("--collect 需要 --game 和 --corpus。\n{USAGE}");
        std::process::exit(2);
    }
    cfg.collect = true;
    task::run_cli(cfg);
}

/// `--translate`：把语料翻成词典。
fn translate(args: &[String]) {
    let cfg = base_config(args);
    if cfg.corpus_path.is_empty() || cfg.out_path.is_empty() {
        eprintln!("--translate 需要 --corpus 和 --out。\n{USAGE}");
        std::process::exit(2);
    }
    match task::translate_cli(&cfg.corpus_path, &cfg.out_path) {
        Ok(msg) => println!("[wtool] {msg}"),
        Err(e) => {
            eprintln!("[wtool] 翻译出错：{e}");
            std::process::exit(2);
        }
    }
}

/// 无子命令的 `--game/--dict`：直接在终端跑翻译循环（旧行为）。
fn plain(args: &[String]) {
    let cfg = base_config(args);
    if cfg.game_dir.is_empty() || cfg.dict_path.is_empty() {
        eprintln!("--game 和 --dict 都得给。\n{USAGE}");
        std::process::exit(2);
    }
    task::run_cli(cfg);
}

/// 从配置文件起手，再用命令行给的逐项覆盖。
///
/// 这是「界面配好一次、脚本一条命令起」的落点：`--host` 因此只需要 `--game`，
/// API Key 也直接从 `config.json` 里取，不必出现在脚本或命令行里。
fn base_config(args: &[String]) -> Config {
    let mut app = AppConfig::load();
    let set = |name: &str, slot: &mut String| {
        if let Some(v) = flag(args, name) {
            *slot = v;
        }
    };
    set("--game", &mut app.game_dir);
    set("--dict", &mut app.dict_path);
    set("--font", &mut app.font);
    set("--corpus", &mut app.corpus_path);
    set("--out", &mut app.out_path);
    set("--base", &mut app.llm_base_url);
    set("--model", &mut app.llm_model);
    set("--key", &mut app.llm_api_key);
    set("--backend", &mut app.translate_backend);
    // `--format` 同时管语料和词典的格式：两者不在同一个命令里同时用到。
    if let Some(v) = flag(args, "--format") {
        app.corpus_format = v.clone();
        app.dict_format = v;
    }
    if let Some(v) = flag(args, "--batch").and_then(|b| b.parse::<usize>().ok()) {
        app.llm_batch = v;
    }
    app.normalize();
    Config::from_app(&app, false)
}

/// 参数里有没有 `--名字`。
fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// 取 `--名字` 后面那个值。没给就返回 `None`。
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}