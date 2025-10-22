//! 通过原生连接对比性能。

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use crossterm::{
    cursor::{MoveTo, MoveToNextLine},
    event::{poll, read, Event, KeyCode, KeyEvent},
    execute,
    style::{Print, Stylize},
    terminal,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
    tty::IsTty,
};
use futures::{
    stream::{self, SelectAll},
    StreamExt,
};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use std::{
    collections::{HashMap, HashSet},
    io,
    io::stdout,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    net::{lookup_host, TcpListener, TcpSocket, TcpStream},
    sync::{mpsc, mpsc::error::TryRecvError, watch},
    task::block_in_place,
};

use aggligator::{
    exec,
    exec::time::{sleep, timeout},
};
use aggligator_monitor::{monitor::format_speed, speed, speed::INTERVAL};
use aggligator_util::init_log;

const PORT: u16 = 5701;
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 为每块网卡单独建立 TCP 连接进行测速。
///
/// 可用于与使用聚合连接的 `agg-speed` 工具进行性能对比，
/// 观察聚合带来的收益。
#[derive(Parser)]
#[command(author, version)]
pub struct RawSpeedCli {
    /// 选择客户端或服务器模式。
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 原生测速客户端。
    Client(RawClientCli),
    /// 原生测速服务器。
    Server(RawServerCli),
}

#[tokio::main]
async fn main() -> Result<()> {
    init_log();
    match RawSpeedCli::parse().command {
        Commands::Client(client) => client.run().await,
        Commands::Server(server) => server.run().await,
    }
}

#[derive(Args)]
pub struct RawClientCli {
    /// 使用 IPv4。
    #[arg(long, short = '4')]
    ipv4: bool,
    /// 使用 IPv6。
    #[arg(long, short = '6')]
    ipv6: bool,
    /// 限制测试传输的数据量（单位：MB）。
    #[arg(long, short = 'l')]
    limit: Option<usize>,
    /// 限制测试持续时间（单位：秒）。
    #[arg(long, short = 't')]
    time: Option<u64>,
    /// 仅测试发送速度。
    #[arg(long, short = 's')]
    send_only: bool,
    /// 仅测试接收速度。
    #[arg(long, short = 'r')]
    recv_only: bool,
    /// 不显示监视界面。
    #[arg(long, short = 'n')]
    no_monitor: bool,
    /// 服务器的名称或 IP 地址与端口号。
    target: Vec<String>,
}

impl RawClientCli {
    async fn resolve_target(&self) -> Result<HashSet<SocketAddr>> {
        if self.ipv4 && self.ipv6 {
            return Err(anyhow!("IPv4 和 IPv6 选项不能同时使用"));
        }

        let mut target = self.target.clone();
        for target in &mut target {
            if !target.contains(':') {
                target.push_str(&format!(":{PORT}"));
            }
        }

        let mut addrs = HashSet::new();

        for target in target {
            for addr in lookup_host(&target).await? {
                if (addr.is_ipv4() && self.ipv6) || (addr.is_ipv6() && self.ipv4) {
                    continue;
                }

                addrs.insert(addr);
            }
        }

        if addrs.is_empty() {
            Err(anyhow!("无法解析目标的 IP 地址"))
        } else {
            Ok(addrs)
        }
    }

    async fn tcp_connect(iface: &[u8], ifaces: &[NetworkInterface], target: SocketAddr) -> Result<TcpStream> {
        let socket = match target.ip() {
            IpAddr::V4(_) => TcpSocket::new_v4(),
            IpAddr::V6(_) => TcpSocket::new_v6(),
        }?;

        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        socket.bind_device(Some(iface))?;
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let _ = ifaces;

        #[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
        {
            let mut bound = false;

            'ifaces: for ifn in ifaces {
                if ifn.name.as_bytes() == iface {
                    for addr in &ifn.addr {
                        match (addr.ip(), target.ip()) {
                            (IpAddr::V4(_), IpAddr::V4(_)) => (),
                            (IpAddr::V6(_), IpAddr::V6(_)) => (),
                            _ => continue,
                        }

                        if addr.ip().is_loopback() != target.ip().is_loopback() {
                            continue;
                        }

                        tracing::debug!("在接口 {} 上绑定地址 {addr:?}", &ifn.name);
                        socket.bind(SocketAddr::new(addr.ip(), 0))?;
                        bound = true;
                        break 'ifaces;
                    }
                }
            }

            if !bound {
                anyhow::bail!("该网卡没有可用的 IP 地址");
            }
        }

        Ok(socket.connect(target).await?)
    }

    #[allow(clippy::type_complexity)]
    async fn test_links(
        targets: HashSet<SocketAddr>, send_only: bool, recv_only: bool, limit: Option<usize>,
        time: Option<Duration>, speeds_tx: Option<mpsc::Sender<(String, Option<(f64, f64)>)>>,
    ) -> Result<()> {
        let mut connected = HashSet::new();
        let (disconnected_tx, mut disconnected_rx) = mpsc::channel(16);

        while !speeds_tx.as_ref().map(|tx| tx.is_closed()).unwrap_or_default() {
            while let Ok(conn) = disconnected_rx.try_recv() {
                connected.remove(&conn);
            }

            let interfaces = NetworkInterface::show().context("无法获取网络接口信息")?;
            let iface_names: HashSet<_> = interfaces.clone().into_iter().map(|iface| iface.name).collect();

            for iface in iface_names {
                for target in &targets {
                    if connected.contains(&(iface.clone(), *target)) {
                        continue;
                    }
                    connected.insert((iface.clone(), *target));

                    let iface = iface.clone();
                    let iface_disconnected_tx = disconnected_tx.clone();
                    let iface_speeds_tx = speeds_tx.clone();
                    let interfaces = interfaces.clone();
                    let target = *target;
                    exec::spawn(async move {
                        if iface_speeds_tx.is_none() {
                            eprintln!("尝试从 {iface} 建立 TCP 连接");
                        }

                        match timeout(
                            TCP_CONNECT_TIMEOUT,
                            Self::tcp_connect(iface.as_bytes(), &interfaces, target),
                        )
                        .await
                        {
                            Ok(Ok(strm)) => {
                                if iface_speeds_tx.is_none() {
                                    eprintln!("已从 {iface} 建立 TCP 连接");
                                }

                                let (read, write) = strm.into_split();
                                let task_iface = iface.clone();

                                let speed_tx = match iface_speeds_tx.clone() {
                                    Some(iface_speeds_tx) => {
                                        let iface = iface.clone();
                                        let (tx, mut rx) = watch::channel(Default::default());
                                        exec::spawn(async move {
                                            while let Ok(()) = rx.changed().await {
                                                let speed = *rx.borrow_and_update();
                                                if iface_speeds_tx
                                                    .send((format!("{iface} -> {target}"), Some(speed)))
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            let _ = iface_speeds_tx.send((iface.clone(), None)).await;
                                        });
                                        Some(tx)
                                    }
                                    None => None,
                                };

                                let _ = speed::speed_test(
                                    &iface, read, write, limit, time, !recv_only, !send_only, false, INTERVAL,
                                    speed_tx,
                                )
                                .await;

                                if iface_speeds_tx.is_none() {
                                    eprintln!("来自 {task_iface} 的 TCP 连接已完成");
                                }
                            }
                            Ok(Err(err)) => {
                                if iface_speeds_tx.is_none() {
                                    eprintln!("来自 {iface} 的 TCP 连接失败：{err}");
                                }
                            }
                            Err(_) => {
                                if iface_speeds_tx.is_none() {
                                    eprintln!("来自 {iface} 的 TCP 连接超时");
                                }
                            }
                        }
                        if iface_speeds_tx.is_none() {
                            eprintln!();
                        }

                        let _ = iface_disconnected_tx.send((iface, target)).await;
                    });
                }
            }

            sleep(Duration::from_secs(3)).await;
        }

        Ok(())
    }

    fn monitor(header: &str, mut speeds_rx: mpsc::Receiver<(String, Option<(f64, f64)>)>) -> Result<()> {
        enable_raw_mode()?;

        let mut speeds = HashMap::new();

        'main: loop {
            loop {
                match speeds_rx.try_recv() {
                    Ok((iface, Some(speed))) => {
                        speeds.insert(iface, speed);
                    }
                    Ok((iface, None)) => {
                        speeds.remove(&iface);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'main,
                }
            }

            let (_cols, rows) = terminal::size().unwrap();
            execute!(stdout(), Clear(ClearType::All), MoveTo(0, 0)).unwrap();
            execute!(stdout(), Print(header.bold()), MoveToNextLine(2)).unwrap();
            execute!(
                stdout(),
                Print("                       上行            下行    ".grey()),
                MoveToNextLine(1)
            )
            .unwrap();

            let mut total_tx = 0.;
            let mut total_rx = 0.;
            for (tx, rx) in speeds.values() {
                total_tx += *tx;
                total_rx += *rx;
            }
            execute!(
                stdout(),
                Print("合计               ".grey()),
                Print(format_speed(total_tx)),
                Print("    "),
                Print(format_speed(total_rx)),
                MoveToNextLine(2),
            )
            .unwrap();

            let mut speeds: Vec<_> = speeds.clone().into_iter().collect();
            speeds.sort_by_key(|(iface, _)| iface.clone());
            for (iface, (tx, rx)) in speeds {
                execute!(
                    stdout(),
                    Print(format!("{iface:20}").cyan()),
                    Print(format_speed(tx)),
                    Print("    "),
                    Print(format_speed(rx)),
                    MoveToNextLine(1),
                )
                .unwrap();
            }

            execute!(stdout(), MoveTo(0, rows - 2), Print("按 q 退出。".to_string().grey()), MoveToNextLine(1))
                .unwrap();

            if poll(Duration::from_secs(1))? {
                if let Event::Key(KeyEvent { code: KeyCode::Char('q'), .. }) = read()? {
                    break;
                }
            }
        }

        disable_raw_mode()?;
        Ok(())
    }

    pub async fn run(mut self) -> Result<()> {
        if !stdout().is_tty() {
            self.no_monitor = true;
        }

        let target = self.resolve_target().await.context("无法解析目标地址")?;
        let header = format!("正在连接位于 {:?} 的原生测速服务", &target);

        let limit = self.limit.map(|mb| mb * 1_048_576);
        let time = self.time.map(Duration::from_secs);

        if self.no_monitor {
            eprintln!("{header}");
            Self::test_links(
                target,
                self.send_only,
                self.recv_only,
                self.limit,
                self.time.map(Duration::from_secs),
                None,
            )
            .await?;
        } else {
            let (speeds_tx, speeds_rx) = mpsc::channel(16);
            exec::spawn(Self::test_links(target, self.send_only, self.recv_only, limit, time, Some(speeds_tx)));
            block_in_place(|| Self::monitor(&header, speeds_rx))?;
        }

        Ok(())
    }
}

#[derive(Args)]
pub struct RawServerCli {
    /// TCP 端口。
    #[arg(default_value_t = PORT)]
    port: u16,
}

impl RawServerCli {
    fn listen(interface: &NetworkInterface, port: u16) -> Result<TcpListener> {
        let addr = SocketAddr::new(interface.addr.first().context("该网卡没有 IP 地址")?.ip(), port);

        let socket = match addr.ip() {
            IpAddr::V4(_) => TcpSocket::new_v4()?,
            IpAddr::V6(_) => TcpSocket::new_v6()?,
        };

        socket.bind(addr)?;

        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        socket.bind_device(Some(interface.name.as_bytes()))?;

        Ok(socket.listen(8)?)
    }

    async fn tcp_serve(port: u16) -> Result<()> {
        let mut listeners = SelectAll::new();

        let interfaces = NetworkInterface::show().context("无法获取网络接口信息")?;
        for interface in interfaces {
            match Self::listen(&interface, port) {
                Ok(listener) => {
                    eprintln!("原生测速服务器监听于 {}", listener.local_addr()?);
                    let stream = stream::try_unfold(listener, |listener| async move {
                        let res = listener.accept().await?;
                        Ok::<_, io::Error>(Some((res, listener)))
                    });
                    listeners.push(stream.boxed());
                }
                Err(err) => {
                    eprintln!("无法在 {interface:?} 上监听：{err}");
                }
            }
        }
        eprintln!();

        while let Some((socket, src)) = listeners.next().await.transpose()? {
            eprintln!("已接受来自 {src} 的 TCP 连接");

            let (read, write) = socket.into_split();
            exec::spawn(async move {
                let _ = speed::speed_test(
                    &src.to_string(),
                    read,
                    write,
                    None,
                    None,
                    true,
                    true,
                    false,
                    INTERVAL,
                    None,
                )
                .await;
                eprintln!("来自 {src} 的 TCP 连接已完成");
            });
        }

        Ok(())
    }

    pub async fn run(self) -> Result<()> {
        Self::tcp_serve(self.port).await
    }
}
