# Aggligator —— 你的友好链路聚合器

[![crates.io page](https://img.shields.io/crates/v/aggligator)](https://crates.io/crates/aggligator)
[![docs.rs page](https://docs.rs/aggligator/badge.svg)](https://docs.rs/aggligator)
[![Apache 2.0 license](https://img.shields.io/crates/l/aggligator)](https://raw.githubusercontent.com/surban/aggligator/master/LICENSE)

Aggligator 可以把多条链路聚合成一条逻辑连接。

它会在两个端点之间同时使用多条网络链路（例如多条 [TCP] 连接），并将其聚合为一条逻辑连接，从而获得所有链路带宽的总和，并在单条链路发生故障时保持连接可用。你可以在运行过程中按需加入或移除链路。

Aggligator 的目标与 [Multipath TCP] 和 [SCTP] 类似，但它完全建立在现有的广泛使用的协议之上，例如 TCP、HTTPS、TLS、USB、WebSocket，并且完全在用户空间实现，无需操作系统提供特殊支持。

Aggligator 完全使用安全的 [Rust] 语言编写，基于 [Tokio] 异步运行时。它能够在主流桌面操作系统以及 WebAssembly 平台上运行。

[TCP]: https://zh.wikipedia.org/wiki/%E4%BC%A0%E8%BE%93%E6%8E%A7%E5%88%B6%E5%8D%8F%E8%AE%AE
[Multipath TCP]: https://en.wikipedia.org/wiki/Multipath_TCP
[SCTP]: https://zh.wikipedia.org/wiki/%E6%B6%A2%E6%B3%A2%E7%8A%B6%E4%BC%A0%E8%BE%93%E5%8D%8F%E8%AE%AE
[Rust]: https://www.rust-lang.org/
[Tokio]: https://tokio.rs/

## 可选特性

下列可选 crate 特性可按需启用：

- `dump` —— 允许将分析数据保存到磁盘，主要用于调试连接性能问题；在某些数据类型上同时启用 [Serde] 支持。
- `js` —— 启用在 JavaScript 运行环境（例如浏览器）中运行所需的支持。

[Serde]: https://serde.rs/

### JavaScript 与 Web 支持

Aggligator 支持编译为 `wasm32-unknown-unknown`、`wasm32-wasip1` 和 `wasm32-wasip1-threads` 三种 WebAssembly 目标。如果你需要在 JavaScript 运行环境（如浏览器）中运行，请务必启用 `js` 特性，以便在浏览器事件循环上调度任务并获得对 JavaScript Promise 的支持。

## 配套 crate

下列 [crate 提供传输层实现]：

- [aggligator-transport-bluer] —— 基于 Linux 的蓝牙传输；
- [aggligator-transport-tcp] —— 基于 TCP 的传输，可选 TLS 加密；
- [aggligator-transport-usb] —— 面向原生平台的 USB 传输；
- [aggligator-transport-webusb] —— 面向 WebAssembly 平台的 WebUSB 传输；
- [aggligator-transport-websocket] —— 面向原生平台的 WebSocket 传输；
- [aggligator-transport-websocket-web] —— 面向 WebAssembly 平台的 WebSocket 传输。

[crate 提供传输层实现]: https://crates.io/keywords/aggligator-transport
[aggligator-transport-bluer]: https://crates.io/crates/aggligator-transport-bluer
[aggligator-transport-tcp]: https://crates.io/crates/aggligator-transport-tcp
[aggligator-transport-usb]: https://crates.io/crates/aggligator-transport-usb
[aggligator-transport-webusb]: https://crates.io/crates/aggligator-transport-webusb
[aggligator-transport-websocket]: https://crates.io/crates/aggligator-transport-websocket
[aggligator-transport-websocket-web]: https://crates.io/crates/aggligator-transport-websocket-web

下列 crate 提供传输包装器：

- [aggligator-wrapper-tls] —— 提供 TLS 安全性的传输包装器。

[aggligator-wrapper-tls]: https://crates.io/crates/aggligator-wrapper-tls

下列 crate 提供实用函数与命令行工具：

- [aggligator-monitor] —— 文本界面的链路监控与测速工具；
- [aggligator-util] —— 包含多种命令行工具，`agg-tunnel` 现已默认启用 CTCP 可打印加密，适合与文本白名单网络或 openppp2 协同。

[aggligator-monitor]: https://crates.io/crates/aggligator-monitor
[aggligator-util]: https://crates.io/crates/aggligator-util

## 示例

两台机器通过以太网和 Wi-Fi 互联。

A 机器（名为 `dino`，运行测速服务端）具有两块网卡：`enp8s0`（千兆以太网，IP 结尾为 `::b01`）与 `wlp6s0`（Wi-Fi，IP 结尾为 `::83e`），两条地址都在 DNS 中注册。

B 机器（运行测速客户端）有四块网卡：`enp0s25`（千兆以太网）、`enxf8eXXXXdd`（USB 千兆以太网）、`enxf8eXXXXc5`（USB 千兆以太网）以及 `wlp3s0`（Wi-Fi）。

在 B 机器上运行 [aggligator-util] crate 中的 `agg-speed` 工具，输出如下图所示：

![Interactive monitor](https://raw.githubusercontent.com/surban/aggligator/master/.misc/monitor.png)

Aggligator 在两台机器之间建立了 8 条链路，也就是 A、B 两侧每块网卡的所有组合。连接的双向传输速度大约为 100 MB/s，与千兆以太网的满双工吞吐量一致。

拔掉任意网线或者关闭 Wi-Fi 会导致剩余链路重新分配流量，但连接保持稳定无中断。当重新插回网线或启用 Wi-Fi 时，对应链路会自动恢复。

## 最低支持的 Rust 版本

最低支持的 Rust 版本（MSRV）为 1.80。

## 授权协议

Aggligator 遵循 [Apache 2.0 许可证]。

[Apache 2.0 许可证]: https://github.com/surban/aggligator/blob/master/LICENSE

### 贡献指南

除非你另行声明，你提交到 Aggligator 的任何贡献都将默认以 Apache 2.0 许可证授权，不附加其它条款或条件。

## 中文使用说明

我们在《[Aggligator 中文使用指南](docs/usage-guide.zh-CN.md)》中提供了更详细的安装、部署与示例说明，并新增了与 `liulilittle/openppp2` 项目协同的 CTCP 文本传输范例；本仓库的 `agg-tunnel` 也已默认启用该 printable 管道，建议在上手之前阅读。
