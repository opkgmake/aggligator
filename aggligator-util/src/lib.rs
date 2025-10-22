#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/surban/aggligator/master/.misc/aggligator.png",
    html_favicon_url = "https://raw.githubusercontent.com/surban/aggligator/master/.misc/aggligator.png",
    issue_tracker_base_url = "https://github.com/surban/aggligator/issues/"
)]

//! Aggligator 命令行工具集。

use anyhow::{bail, Context};
use std::path::PathBuf;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use aggligator::cfg::Cfg;
use aggligator_transport_tcp::TcpLinkFilter;

/// 为命令行工具初始化日志系统。
pub fn init_log() {
    tracing_subscriber::registry().with(fmt::layer()).with(EnvFilter::from_default_env()).init();
    tracing_log::LogTracer::init().unwrap();
}

/// 打印 Aggligator 的默认配置。
pub fn print_default_cfg() {
    println!("{}", serde_json::to_string_pretty(&Cfg::default()).unwrap());
}

/// 从磁盘加载 Aggligator 配置；当未提供路径时返回默认配置。
pub fn load_cfg(path: &Option<PathBuf>) -> anyhow::Result<Cfg> {
    match path {
        Some(path) => {
            let file = std::fs::File::open(path).context("无法打开配置文件")?;
            serde_json::from_reader(file).context("无法解析配置文件")
        }
        None => Ok(Cfg::default()),
    }
}

/// 解析 [TcpLinkFilter] 选项。
pub fn parse_tcp_link_filter(s: &str) -> anyhow::Result<TcpLinkFilter> {
    match s {
        "none" => Ok(TcpLinkFilter::None),
        "interface-interface" => Ok(TcpLinkFilter::InterfaceInterface),
        "interface-ip" => Ok(TcpLinkFilter::InterfaceIp),
        other => bail!("未知的 TCP 链路过滤器：{other}"),
    }
}

/// 等待平台相关的终止信号。
pub async fn wait_sigterm() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = signal(SignalKind::terminate()).unwrap();
        let mut sigint = signal(SignalKind::interrupt()).unwrap();
        let mut sighup = signal(SignalKind::hangup()).unwrap();

        tokio::select! {
            _ = sigterm.recv() => {},
            _ = sigint.recv() => {},
            _ = sighup.recv() => {},
        }
    }

    #[cfg(windows)]
    {
        use tokio::signal::windows;

        let mut ctrl_c = windows::ctrl_c().unwrap();
        let mut ctrl_break = windows::ctrl_break().unwrap();
        let mut ctrl_close = windows::ctrl_close().unwrap();
        let mut ctrl_logoff = windows::ctrl_logoff().unwrap();
        let mut ctrl_shutdown = windows::ctrl_shutdown().unwrap();

        tokio::select! {
            _ = ctrl_c.recv() => {},
            _ = ctrl_break.recv() => {},
            _ = ctrl_close.recv() => {},
            _ = ctrl_logoff.recv() => {},
            _ = ctrl_shutdown.recv() => {},
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        std::future::pending::<()>().await;
    }

    tracing::info!("收到终止信号");
}
