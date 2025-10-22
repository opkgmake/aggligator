//! Aggligator 速度测试工具。

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use crossterm::{style::Stylize, tty::IsTty};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme,
};
use rustls_pemfile::{certs, private_key};
use serde::Serialize;
use std::{
    collections::HashSet,
    io::{stdout, BufReader},
    net::{Ipv6Addr, SocketAddr},
    path::PathBuf,
    process::exit,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::block_in_place,
    time::sleep,
};

use aggligator::{
    cfg::Cfg,
    dump::dump_to_json_line_file,
    exec,
    transport::{AcceptorBuilder, ConnectorBuilder, LinkTagBox},
};
use aggligator_monitor::{
    monitor::{format_speed, interactive_monitor},
    speed::{speed_test, INTERVAL},
};
use aggligator_transport_tcp::{IpVersion, TcpAcceptor, TcpConnector, TcpLinkFilter};
use aggligator_transport_websocket::{WebSocketAcceptor, WebSocketConnector};
use aggligator_util::{init_log, load_cfg, parse_tcp_link_filter, print_default_cfg, wait_sigterm};
use aggligator_wrapper_tls::{TlsClient, TlsServer};

#[cfg(feature = "bluer")]
use aggligator_transport_bluer::rfcomm::{RfcommAcceptor, RfcommConnector};
#[cfg(feature = "bluer")]
use aggligator_transport_bluer::rfcomm_profile::{RfcommProfileAcceptor, RfcommProfileConnector};

#[cfg(feature = "usb-device")]
use aggligator_transport_usb::{upc, usb_gadget};

const TCP_PORT: u16 = 5700;
const DUMP_BUFFER: usize = 8192;

const WEBSOCKET_PORT: u16 = 8080;
const WEBSOCKET_PATH: &str = "/agg-speed";

#[cfg(any(feature = "usb-host", feature = "usb-device"))]
mod usb {
    pub const VID: u16 = u16::MAX - 1;
    pub const PID: u16 = u16::MAX - 1;
    pub const MANUFACTURER: &str = env!("CARGO_PKG_NAME");
    pub const PRODUCT: &str = env!("CARGO_BIN_NAME");
    pub const CLASS: u8 = 255;
    pub const SUB_CLASS: u8 = 255;
    pub const PROTOCOL: u8 = 255;
    pub const INTERFACE_CLASS: u8 = 255;
    pub const INTERFACE_SUB_CLASS: u8 = 230;
    pub const INTERFACE_PROTOCOL: u8 = 231;
    pub const INTERFACE_NAME: &str = "速度测试";
}

#[cfg(feature = "bluer")]
const RFCOMM_CHANNEL: u8 = 20;
#[cfg(feature = "bluer")]
const RFCOMM_UUID: aggligator_transport_bluer::rfcomm_profile::Uuid =
    aggligator_transport_bluer::rfcomm_profile::Uuid::from_u128(0x7f95058c_c00e_44a9_9003_2ce90d60e2e7);

static TLS_CERT_PEM: &[u8] = include_bytes!("agg-speed-cert.pem");
static TLS_KEY_PEM: &[u8] = include_bytes!("agg-speed-key.pem");
static TLS_SERVER_NAME: &str = "aggligator.rs";

fn tls_cert() -> CertificateDer<'static> {
    let mut reader = BufReader::new(TLS_CERT_PEM);
    let mut certs = certs(&mut reader);
    certs.next().unwrap().unwrap()
}

fn tls_key() -> PrivateKeyDer<'static> {
    let mut reader = BufReader::new(TLS_KEY_PEM);
    private_key(&mut reader).unwrap().unwrap()
}

/// 接受任意 TLS 服务器证书。
///
/// 仅供速度测试使用，切勿用于生产环境！
#[derive(Debug)]
struct TlsNullVerifier;

impl ServerCertVerifier for TlsNullVerifier {
    fn verify_server_cert(
        &self, _end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>, _ocsp_response: &[u8], _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>, _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>, _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

fn tls_client_config() -> ClientConfig {
    let mut root_store = RootCertStore::empty();
    root_store.add(tls_cert()).unwrap();
    let mut cfg = ClientConfig::builder().with_root_certificates(root_store).with_no_client_auth();
    cfg.dangerous().set_certificate_verifier(Arc::new(TlsNullVerifier));
    cfg
}

fn tls_server_config() -> ServerConfig {
    ServerConfig::builder().with_no_client_auth().with_single_cert(vec![tls_cert()], tls_key()).unwrap()
}

fn debug_warning() -> String {
    match cfg!(debug_assertions) {
        true => "⚠ 调试构建：速度会偏慢 ⚠\n".red().to_string(),
        false => String::new(),
    }
}

/// 使用聚合的 TCP 链路运行速度测试。
///
/// Aggligator 会将多条 TCP 链路合并为一个逻辑连接，
/// 既汇聚所有链路的带宽，也能在单条链路故障时保持连接稳定。
#[derive(Parser)]
#[command(name = "agg-speed", author, version)]
pub struct SpeedCli {
    /// 配置文件。
    #[arg(long)]
    cfg: Option<PathBuf>,
    /// 将分析数据写入文件。
    #[arg(long, short = 'd')]
    dump: Option<PathBuf>,
    /// 选择客户端或服务器模式。
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 聚合链路速度测试客户端。
    Client(ClientCli),
    /// 聚合链路速度测试服务器。
    Server(ServerCli),
    /// 显示默认配置。
    ShowCfg,
    /// 在当前目录生成该工具的手册页。
    #[command(hide = true)]
    ManPages,
    /// 生成该工具的 Markdown 帮助文档。
    #[command(hide = true)]
    Markdown,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_log();

    let cli = SpeedCli::parse();
    let cfg = load_cfg(&cli.cfg)?;
    let dump = cli.dump.clone();

    if cfg!(debug_assertions) {
        eprintln!("{}", debug_warning());
    }

    let res = match cli.command {
        Commands::Client(client) => client.run(cfg, dump).await,
        Commands::Server(server) => server.run(cfg, dump).await,
        Commands::ShowCfg => {
            print_default_cfg();
            Ok(())
        }
        Commands::ManPages => clap_mangen::generate_to(SpeedCli::command(), ".").map_err(|err| err.into()),
        Commands::Markdown => {
            println!("{}", clap_markdown::help_markdown::<SpeedCli>());
            Ok(())
        }
    };

    sleep(Duration::from_millis(300)).await;
    tracing::debug!("主程序退出");
    res
}

#[derive(Parser)]
pub struct ClientCli {
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
    /// 阻塞接收端。
    #[arg(long, short = 'b')]
    recv_block: bool,
    /// 不显示链路监视器。
    #[arg(long, short = 'n')]
    no_monitor: bool,
    /// 在监视器中显示所有可能的链路（包括未连接的链路）。
    #[arg(long, short = 'a')]
    all_links: bool,
    /// 以 JSON 格式输出速度报告。
    #[arg(long, short = 'j')]
    json: bool,
    /// 使用 TLS 加密所有链路，不校验服务器身份。
    ///
    /// 警告：不会执行任何服务器身份验证！
    #[arg(long)]
    tls: bool,
    /// TCP 服务器的名称或 IP 地址与端口号。
    #[arg(long)]
    tcp: Vec<String>,
    /// TCP 链路过滤方式。
    ///
    /// none：不过滤任何链路。
    ///
    /// interface-interface：为每对本地和远端网卡创建一条链路。
    ///
    /// interface-ip：为每个本地网卡与远端 IP 的组合创建一条链路。
    #[arg(long, value_parser = parse_tcp_link_filter, default_value = "interface-interface")]
    tcp_link_filter: TcpLinkFilter,
    /// WebSocket 主机或 URL。
    ///
    /// 默认端口为 8080，路径为 /agg-speed。
    #[arg(long)]
    websocket: Vec<String>,
    /// 蓝牙 RFCOMM 服务器地址。
    #[cfg(feature = "bluer")]
    #[arg(long, value_parser=parse_rfcomm)]
    rfcomm: Option<aggligator_transport_bluer::rfcomm::SocketAddr>,
    /// 蓝牙 RFCOMM Profile 服务器地址。
    #[cfg(feature = "bluer")]
    #[arg(long)]
    rfcomm_profile: Option<aggligator_transport_bluer::rfcomm_profile::Address>,
    /// USB 设备序列号（等同于测速设备的主机名）。
    #[cfg(feature = "usb-host")]
    #[arg(long)]
    usb: Option<String>,
}

#[cfg(feature = "bluer")]
fn parse_rfcomm(arg: &str) -> Result<aggligator_transport_bluer::rfcomm::SocketAddr> {
    match arg.parse::<aggligator_transport_bluer::rfcomm::SocketAddr>() {
        Ok(addr) => Ok(addr),
        Err(err) => match arg.parse::<aggligator_transport_bluer::rfcomm::Address>() {
            Ok(addr) => Ok(aggligator_transport_bluer::rfcomm::SocketAddr::new(addr, RFCOMM_CHANNEL)),
            Err(_) => Err(err.into()),
        },
    }
}

impl ClientCli {
    pub async fn run(mut self, cfg: Cfg, dump: Option<PathBuf>) -> Result<()> {
        if !stdout().is_tty() {
            self.no_monitor = true;
        }

        let mut builder = ConnectorBuilder::new(cfg);
        if let Some(dump) = dump.clone() {
            let (tx, rx) = mpsc::channel(DUMP_BUFFER);
            builder.task().dump(tx);
            exec::spawn(dump_to_json_line_file(dump, rx));
        }
        if self.tls {
            builder.wrap(TlsClient::new(
                Arc::new(tls_client_config()),
                ServerName::try_from(TLS_SERVER_NAME).unwrap(),
            ));
        }

        let mut connector = builder.build();
        let mut targets = Vec::new();
        let ip_version = IpVersion::from_only(self.ipv4, self.ipv6)?;

        if !self.tcp.is_empty() {
            let mut tcp_connector =
                TcpConnector::new(self.tcp.clone(), TCP_PORT).await.context("无法解析 TCP 目标")?;
            tcp_connector.set_ip_version(ip_version);
            tcp_connector.set_link_filter(self.tcp_link_filter);
            targets.push(tcp_connector.to_string());
            connector.add(tcp_connector);
        }

        #[cfg(feature = "bluer")]
        if let Some(addr) = self.rfcomm {
            let rfcomm_connector = RfcommConnector::new(addr);
            targets.push(addr.to_string());
            connector.add(rfcomm_connector);
        }

        #[cfg(feature = "bluer")]
        if let Some(addr) = self.rfcomm_profile {
            let rfcomm_profile_connector = RfcommProfileConnector::new(addr, RFCOMM_UUID)
                .await
                .context("RFCOMM Profile 连接器初始化失败")?;
            targets.push(addr.to_string());
            connector.add(rfcomm_profile_connector);
        }

        #[cfg(feature = "usb-host")]
        if let Some(serial) = &self.usb {
            let filter_serial = serial.clone();
            let filter = move |dev: &aggligator_transport_usb::DeviceInfo,
                               iface: &aggligator_transport_usb::InterfaceInfo| {
                dev.vendor_id == usb::VID
                    && dev.product_id == usb::PID
                    && dev.manufacturer.as_deref() == Some(usb::MANUFACTURER)
                    && dev.product.as_deref() == Some(usb::PRODUCT)
                    && dev.serial_number.as_deref() == Some(filter_serial.as_str())
                    && dev.class_code == usb::CLASS
                    && dev.sub_class_code == usb::SUB_CLASS
                    && dev.protocol_code == usb::PROTOCOL
                    && iface.class_code == usb::INTERFACE_CLASS
                    && iface.sub_class_code == usb::INTERFACE_SUB_CLASS
                    && iface.protocol_code == usb::INTERFACE_PROTOCOL
                    && iface.description.as_deref() == Some(usb::INTERFACE_NAME)
            };
            let usb_connector = aggligator_transport_usb::UsbConnector::new(filter).context("无法初始化 USB")?;
            targets.push(format!("USB 设备 {serial}"));
            connector.add(usb_connector);
        }

        if !self.websocket.is_empty() {
            let websockets = self.websocket.iter().map(|url| {
                let mut url = url.clone();
                if !url.starts_with("ws") {
                    url = format!("ws://{url}:{WEBSOCKET_PORT}{WEBSOCKET_PATH}");
                }
                url
            });
            let mut ws_connector =
                WebSocketConnector::new(websockets).await.context("无法解析 WebSocket 目标")?;
            ws_connector.set_ip_version(ip_version);
            targets.push(ws_connector.to_string());
            connector.add(ws_connector);
        }

        if targets.is_empty() {
            bail!("未指定任何连接传输方式。");
        }

        let target = targets.join(", ");
        let title = format!("对 {target} 进行速度测试{}", if self.tls { "（启用 TLS）" } else { "" });

        let outgoing = connector.channel().unwrap();
        let control = connector.control();

        exec::spawn({
            let control = control.clone();
            async move {
                wait_sigterm().await;
                control.terminate();
            }
        });

        let tags_rx = connector.available_tags_watch();
        let tag_err_rx = connector.link_errors();
        let (disabled_tags_tx, mut disabled_tags_rx) = watch::channel(HashSet::new());
        exec::spawn(async move {
            loop {
                let disabled_tags: HashSet<LinkTagBox> = (*disabled_tags_rx.borrow_and_update()).clone();
                connector.set_disabled_tags(disabled_tags);

                if disabled_tags_rx.changed().await.is_err() {
                    break;
                }
            }
        });

        let (control_tx, control_rx) = broadcast::channel(8);
        let (header_tx, header_rx) = watch::channel(Default::default());
        let (speed_tx, mut speed_rx) = watch::channel(Default::default());

        let _ = control_tx.send((control.clone(), String::new()));
        drop(control_tx);

        if !self.no_monitor {
            exec::spawn(async move {
                loop {
                    let (send, recv) = *speed_rx.borrow_and_update();
                    let speed = format!(
                        "{}{}\r\n{}{}\r\n",
                        "上行：   ".grey(),
                        format_speed(send),
                        "下行： ".grey(),
                        format_speed(recv)
                    );
                    let header = format!("{}\r\n\r\n{}{}", title.clone().bold(), speed, debug_warning());

                    if header_tx.send(header).is_err() {
                        break;
                    }

                    if speed_rx.changed().await.is_err() {
                        break;
                    }
                }
            });
        }

        let speed_test = async move {
            let ch = outgoing.await.context("无法建立 Aggligator 连接")?;
            let (r, w) = ch.into_stream().into_split();
            anyhow::Ok(
                speed_test(
                    &target,
                    r,
                    w,
                    self.limit.map(|mb| mb * 1_048_576),
                    self.time.map(Duration::from_secs),
                    !self.recv_only,
                    !self.send_only,
                    self.recv_block,
                    INTERVAL,
                    if self.no_monitor { None } else { Some(speed_tx) },
                )
                .await?,
            )
        };

        let (tx_speed, rx_speed) = if self.no_monitor {
            drop(tag_err_rx);
            let res = speed_test.await;
            res?
        } else {
            let task = exec::spawn(speed_test);
            block_in_place(|| {
                interactive_monitor(
                    header_rx,
                    control_rx,
                    1,
                    self.all_links.then_some(tags_rx),
                    Some(tag_err_rx),
                    self.all_links.then_some(disabled_tags_tx),
                )
            })?;

            task.abort();
            match task.await {
                Ok(res) => res?,
                Err(_) => {
                    println!("正在退出…");
                    control.terminated().await?;
                    return Ok(());
                }
            }
        };

        if self.json {
            let report = SpeedReport {
                data_limit: self.limit,
                time_limit: self.time,
                send_speed: tx_speed,
                recv_speed: tx_speed,
            };
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
        } else {
            println!("上行速度：{}", format_speed(tx_speed));
            println!("下行速度：{}", format_speed(rx_speed));
        }

        println!("正在退出…");
        control.terminated().await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedReport {
    data_limit: Option<usize>,
    time_limit: Option<u64>,
    send_speed: f64,
    recv_speed: f64,
}

#[derive(Parser)]
pub struct ServerCli {
    /// 在每块网卡上分别监听。
    #[arg(long, short = 'i')]
    individual_interfaces: bool,
    /// 不显示链路监视器。
    #[arg(long, short = 'n')]
    no_monitor: bool,
    /// 处理完一条连接后立即退出。
    #[arg(long)]
    oneshot: bool,
    /// 使用 TLS 加密所有链路。
    #[arg(long)]
    tls: bool,
    /// 监听的 TCP 端口。
    #[arg(long, default_value_t = TCP_PORT)]
    tcp: u16,
    /// 要监听的 RFCOMM 信道号。
    #[cfg(feature = "bluer")]
    #[arg(long, default_value_t = RFCOMM_CHANNEL)]
    rfcomm: u8,
    /// 监听 USB 设备控制器（UDC）。
    #[cfg(feature = "usb-device")]
    #[arg(long)]
    usb: bool,
    /// 要监听的 WebSocket（HTTP）端口。
    #[arg(long, default_value_t = WEBSOCKET_PORT)]
    websocket: u16,
}

impl ServerCli {
    pub async fn run(mut self, cfg: Cfg, dump: Option<PathBuf>) -> Result<()> {
        if !stdout().is_tty() {
            self.no_monitor = true;
        }

        let mut builder = AcceptorBuilder::new(cfg);
        if let Some(dump) = dump {
            builder.set_task_cfg(move |task| {
                let (tx, rx) = mpsc::channel(DUMP_BUFFER);
                task.dump(tx);
                exec::spawn(dump_to_json_line_file(dump.clone(), rx));
            });
        }
        if self.tls {
            builder.wrap(TlsServer::new(Arc::new(tls_server_config())));
        }

        let acceptor = builder.build();
        let mut ports = Vec::new();

        let tcp_acceptor_res = if self.individual_interfaces {
            TcpAcceptor::all_interfaces(self.tcp).await
        } else {
            TcpAcceptor::new([SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), self.tcp)]).await
        };
        match tcp_acceptor_res {
            Ok(tcp) => {
                ports.push(format!("TCP 端口 {tcp}"));
                acceptor.add(tcp);
            }
            Err(err) => eprintln!("无法监听 TCP 端口 {}：{err}", self.tcp),
        }

        #[cfg(feature = "bluer")]
        match RfcommAcceptor::new(aggligator_transport_bluer::rfcomm::SocketAddr::new(
            aggligator_transport_bluer::rfcomm::Address::any(),
            self.rfcomm,
        ))
        .await
        {
            Ok(rfcomm) => {
                acceptor.add(rfcomm);
                ports.push(format!("RFCOMM 信道 {}", self.rfcomm));
            }
            Err(err) => eprintln!("无法监听 RFCOMM 信道 {}：{err}", self.rfcomm),
        }

        #[cfg(feature = "bluer")]
        match RfcommProfileAcceptor::new(RFCOMM_UUID).await {
            Ok(rfcomm_profile) => {
                acceptor.add(rfcomm_profile);
                ports.push("RFCOMM Profile 服务".to_string());
            }
            Err(err) => eprintln!("无法监听 RFCOMM Profile {RFCOMM_UUID}：{err}"),
        }

        #[cfg(feature = "usb-device")]
        let _usb_reg = if self.usb {
            fn register_usb(
                serial: &str,
            ) -> Result<(usb_gadget::RegGadget, upc::device::UpcFunction, std::ffi::OsString)> {
                let udc = usb_gadget::default_udc()?;
                let udc_name = udc.name().to_os_string();

                let (upc, func_hnd) = upc::device::UpcFunction::new(
                    upc::device::InterfaceId::new(upc::Class::new(
                        usb::INTERFACE_CLASS,
                        usb::INTERFACE_SUB_CLASS,
                        usb::INTERFACE_PROTOCOL,
                    ))
                    .with_name(usb::INTERFACE_NAME),
                );

                let reg = usb_gadget::Gadget::new(
                    usb_gadget::Class::new(usb::CLASS, usb::SUB_CLASS, usb::PROTOCOL),
                    usb_gadget::Id::new(usb::VID, usb::PID),
                    usb_gadget::Strings::new(usb::MANUFACTURER, usb::PRODUCT, serial),
                )
                .with_os_descriptor(usb_gadget::OsDescriptor::microsoft())
                .with_config(usb_gadget::Config::new("config").with_function(func_hnd))
                .bind(&udc)?;

                Ok((reg, upc, udc_name))
            }

            let serial = gethostname::gethostname().to_string_lossy().to_string();
            match register_usb(&serial) {
                Ok((usb_reg, upc, udc_name)) => {
                    acceptor.add(aggligator_transport_usb::UsbAcceptor::new(upc, &udc_name));
                    ports.push(format!("UDC {}（{serial}）", udc_name.to_string_lossy()));
                    Some(usb_reg)
                }
                Err(err) => {
                    eprintln!("无法监听 USB：{err}");
                    None
                }
            }
        } else {
            None
        };

        let (wsa, router) = WebSocketAcceptor::new(WEBSOCKET_PATH);
        acceptor.add(wsa);
        ports.push(format!("WebSocket 端口 {}", self.websocket));
        let websocket_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), self.websocket);
        exec::spawn(async move {
            if let Err(err) = axum_server::bind(websocket_addr)
                .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                .await
            {
                eprintln!("无法监听 WebSocket {websocket_addr}：{err}");
            }
        });

        if ports.is_empty() {
            bail!("未配置任何监听传输方式。");
        }

        let ports = ports.join(", ");
        let title = format!("速度测试服务监听于 {ports}{}", if self.tls { "（启用 TLS）" } else { "" });

        let tag_error_rx = acceptor.link_errors();
        let (control_tx, control_rx) = broadcast::channel(8);
        let no_monitor = self.no_monitor;
        let oneshot = self.oneshot;
        let task = async move {
            let term_tx = broadcast::Sender::<()>::new(1);
            loop {
                let (ch, control) = tokio::select! {
                    res = acceptor.accept() => res?,
                    () = wait_sigterm() => break,
                };
                exec::spawn({
                    let control = control.clone();
                    let mut term_rx = term_tx.subscribe();
                    async move {
                        let _ = term_rx.recv().await;
                        control.terminate();
                    }
                });
                let _ = control_tx.send((control, String::new()));

                exec::spawn(async move {
                    let id = ch.id();
                    let (r, w) = ch.into_stream().into_split();
                    let (speed_tx, _speed_rx) = watch::channel(Default::default());
                    let speed_tx_opt = if no_monitor { None } else { Some(speed_tx) };
                    let res =
                        speed_test(&id.to_string(), r, w, None, None, true, true, false, INTERVAL, speed_tx_opt)
                            .await;
                    if oneshot {
                        exit(res.is_err() as _);
                    }
                });
            }

            anyhow::Ok(())
        };

        if self.no_monitor {
            task.await?;
        } else {
            let task = exec::spawn(task);

            let header_rx = watch::channel(format!("{}\r\n{}", title.bold(), debug_warning())).1;
            block_in_place(|| interactive_monitor(header_rx, control_rx, 1, None, Some(tag_error_rx), None))?;

            task.abort();
            if let Ok(res) = task.await {
                res?
            }
        }

        Ok(())
    }
}
