//! lanecho-cli: 协议联调与验证工具(M1 里程碑交付物)
//!
//! 在 Tauri UI 就绪前, 用于在两台机器(或同机双实例)间验证节点发现、
//! 配对与剪贴板互同步。同机双实例: 各指定独立 --data-dir 且 --port 0。

mod commands;
mod output;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// 命令行定义: 子命令 + 全局选项(clap 自动生成帮助与错误提示)
#[derive(Parser)]
#[command(
    name = "lanecho-cli",
    version,
    about = "lanecho — 局域网剪贴板同步联调工具",
    after_help = "<目标> 形式: 节点名称 | 设备ID | 指纹前缀\n\
        listen 的标准输入: 普通行 = 注入文本(模拟复制并广播);\n\
        /pair <目标> = 发起配对(推荐在常驻 listen 内配对, 即时生效);\n\
        /y /n = 回应配对请求; /peers /paired = 快照; /quit = 退出"
)]
struct Cli {
    /// 身份数据目录(默认 ~/.lanecho)
    #[arg(long, global = true, value_name = "目录")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

/// 子命令
#[derive(Subcommand)]
enum Command {
    /// 显示本机身份
    Id,
    /// 驻留节点: 发现 + 配对受理 + 剪贴板互同步
    Listen {
        /// 监听端口(0 = 随机, 同机多实例用)
        #[arg(long, value_name = "端口", default_value_t = lanecho_core::DEFAULT_TCP_PORT)]
        port: u16,
        /// 自动接受全部配对请求
        #[arg(long = "yes")]
        auto_accept: bool,
        /// 不接管系统剪贴板(纯 stdin 注入 + 打印, 同机联调防串扰)
        #[arg(long = "no-clipboard")]
        no_clipboard: bool,
    },
    /// 扫描在线节点(默认 6s)
    Scan {
        /// 等待秒数
        #[arg(long = "wait", value_name = "秒数", default_value_t = 6)]
        wait_secs: u64,
    },
    /// 向指定节点发起配对(等待对方确认)
    Pair {
        /// 目标(名称/指纹前缀)
        #[arg(long = "to", value_name = "目标")]
        target: String,
        /// 搜索目标的等待秒数
        #[arg(long = "wait", value_name = "秒数", default_value_t = 10)]
        wait_secs: u64,
    },
    /// 监视本机剪贴板并打印变化事件(无网络, 验证 watcher)
    Watch,
}

/// 通用参数(解析后传给各子命令实现)
struct CommonArgs {
    /// 身份数据目录
    data_dir: PathBuf,
}

/// 入口: 初始化日志, 解析参数并分发子命令
#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let cli = Cli::parse();
    let common = CommonArgs {
        data_dir: cli.data_dir.unwrap_or_else(default_data_dir),
    };
    match cli.command {
        Command::Id => commands::cmd_id(&common).await,
        Command::Listen {
            port,
            auto_accept,
            no_clipboard,
        } => commands::cmd_listen(&common, port, auto_accept, !no_clipboard).await,
        Command::Scan { wait_secs } => commands::cmd_scan(&common, wait_secs).await,
        Command::Pair { target, wait_secs } => {
            commands::cmd_pair(&common, &target, wait_secs).await
        }
        Command::Watch => commands::cmd_watch().await,
    }
}

/// 初始化 tracing 日志: 输出到 stderr, 级别由 RUST_LOG 控制(默认 warn)
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// 用户主目录(HOME / USERPROFILE), 兜底当前目录
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 默认身份数据目录: ~/.lanecho
fn default_data_dir() -> PathBuf {
    home_dir().join(".lanecho")
}
