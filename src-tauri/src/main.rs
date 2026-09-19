// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 两个入口，看命令行参数分流：
//!
//! - 不带参数 → 开窗口，就是平时那个图形界面。
//! - 带 `--game <目录> --dict <文件>` → 不开窗口，直接在终端里跑，
//!   等价于界面上点「开始翻译」，Ctrl+C 相当于点「停止」。
//!
//! 命令行模式请用 `cargo run -- ...`（debug）跑：上面那句
//! `windows_subsystem = "windows"` 只在 release 生效，release 下没有终端可看。

use wtool_lib::task::Config;

const USAGE: &str = "用法：wtool --game <游戏目录> --dict <词典文件>
不带参数则打开图形界面。";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        wtool_lib::run();
        return;
    }

    wtool_lib::task::run_cli(parse(&args));
}

/// 认这两个参数。给不齐就带着用法退出 —— 不能悄悄退回去开界面，
/// 那样「少写一个参数」的表现是弹个窗口，太难查了。
fn parse(args: &[String]) -> Config {
    let mut game_dir = None;
    let mut dict_path = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--game" => game_dir = it.next().cloned(),
            "--dict" => dict_path = it.next().cloned(),
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
        },
        _ => {
            eprintln!("--game 和 --dict 都得给。\n{USAGE}");
            std::process::exit(2);
        }
    }
}
