# Aggligator 中文使用指南

本指南概述 Aggligator 生态系统的安装、常用命令以及典型使用场景。阅读完后，你可以在自己的网络环境中搭建链路聚合、测速与 TCP 隧道转发。

## 1. 基本概念与组件

Aggligator 由以下几个部分组成：

- **核心库 (`aggligator`)**：提供链路聚合逻辑，可在你自己的 Rust 项目中直接使用。
- **命令行工具 (`aggligator-util`)**：包含 `agg-speed` 与 `agg-tunnel` 两个实用程序，用于测速与端口转发。
- **可选传输插件**：根据运行平台选择 TCP、WebSocket、USB、蓝牙等不同传输层。

在大多数入门场景下，只需安装 `aggligator-util` 并使用 TCP 传输即可。

## 2. 环境准备

1. 安装 Rust 工具链（要求 Rust 1.80 或更新）。官方推荐使用 [rustup](https://rustup.rs/)。
2. 确认 `cargo` 已添加到 `PATH` 中：
   ```bash
   cargo --version
   ```
3. （可选）克隆源码仓库便于参考示例或自行构建：
   ```bash
   git clone https://github.com/surban/aggligator.git
   cd aggligator
   ```

## 3. 安装或构建工具

### 3.1 直接安装命令行工具

如果只需使用现成的命令行工具，在任意主机上执行：

```bash
cargo install aggligator-util
```

安装完成后，`agg-speed` 与 `agg-tunnel` 会被放置在 `~/.cargo/bin/` 目录中。

### 3.2 从源码构建（可选）

如需调试或自定义功能，可在仓库根目录构建全部 crate：

```bash
cargo build --release
```

编译完成后，可执行文件位于 `target/release/` 目录，例如 `target/release/agg-speed`。

## 4. 快速上手：多链路测速

以下示例展示如何在两台主机之间进行聚合测速。假设两台主机均可互相访问，并且网络拓扑允许多个独立的 TCP 链路同时存在。

### 4.1 在服务端启动 `agg-speed`

在服务端主机运行：

```bash
agg-speed server --tcp 5700 --websocket 8080
```

- `--tcp 5700`：指定监听 TCP 端口（默认即为 5700，可省略）。
- `--websocket 8080`：额外提供基于 WebSocket 的监听端口，可用于浏览器或受限环境。
- 如需监听每个网卡的独立地址，可追加 `--individual-interfaces`。

终端会出现动态的链路监控界面，展示当前接入的链路数量和吞吐量。

### 4.2 在客户端运行测速

在客户端主机运行：

```bash
agg-speed client --tcp 203.0.113.10:5700 --time 60
```

- `--tcp` 指定服务端的地址列表，可提供一个或多个 `host:port`；多个地址之间使用逗号分隔，例如 `server-a:5700,server-b:5700`。
- `--time 60` 表示测速持续 60 秒。也可以使用 `--limit` 指定传输的数据量（单位 MB）。
- 如需输出 JSON 报告供机器读取，添加 `--json`。
- 若要仅测上传或下载，可分别添加 `--send-only` 或 `--recv-only`。

运行过程中，你将在终端看到实时的链路状态、每条链路的吞吐量以及总体速度统计。

### 4.3 TLS 加密（可选）

若需要在公网环境中运行，可开启 TLS：

- 服务端添加 `--tls` 以启用 TLS 监听。
- 客户端添加 `--tls` 以使用加密传输（默认不验证证书，适合内网实验）。

如需更严格的认证，可结合 `aggligator-wrapper-tls` crate 自定义证书管理流程。

## 5. 使用 `agg-tunnel` 进行端口转发

`agg-tunnel` 可以把多个链路聚合成一条加速隧道，从服务端转发 TCP 端口到客户端。

### 5.1 启动服务端

在需暴露应用端口的主机上运行：

```bash
agg-tunnel server --tcp 5700 --port 22 --port 3389
```

- `--port` 可重复使用，用于声明希望转发给客户端的端口列表。
- 可以与 `agg-speed server` 一样使用 `--tcp` 指定监听端口。

### 5.2 启动客户端

在需要访问这些端口的主机上运行：

```bash
agg-tunnel client --tcp server.example.com:5700 --port 22:10022 --port 3389
```

- `--port` 支持两种写法：
  - `远端端口`：将远端端口映射到本地同名端口；
  - `远端端口:本地端口`：自定义本地监听端口，例如上例将远端 `22` 转发为本地 `10022`。
- 若希望在所有网卡上开放本地端口，可添加 `--global`。
- 若要在客户端上看到所有潜在链路（含未连接的），可使用 `--all-links`。

客户端启动后，可在本地通过 `ssh localhost -p 10022` 等命令访问相应服务，流量会自动分布到多条底层链路上。

## 6. 配置文件与默认值

`agg-speed` 与 `agg-tunnel` 均支持 `--cfg <路径>` 参数，可加载 YAML 配置文件。你可以先运行以下命令查看默认配置并作为模板：

```bash
agg-speed show-cfg > agg-speed.yaml
agg-tunnel show-cfg > agg-tunnel.yaml
```

在生成的文件中，根据注释修改链路、传输、TLS 等设置，然后通过 `--cfg` 指定配置文件即可。命令行参数会覆盖配置文件中的同名字段。

## 7. 故障排查建议

- **链路数少于预期**：确认双方主机能互相访问所有网卡的 IP，防火墙允许对应端口，并检查是否启用了 `--individual-interfaces`。
- **性能不理想**：使用 `agg-speed client --dump output.json` 导出分析数据，在支持 Serde 的工具中查看瓶颈；同时关注 CPU 占用、链路 RTT、丢包情况。
- **连接偶发中断**：确认各条链路的稳定性，必要时减少不稳定的接口，或调整配置以延长重试与超时时间。

## 8. 更多资源

- 项目主页与英文文档：参见仓库根目录 `README.md`。
- 传输插件与 TLS 包装器的详细 API：查阅各个 crate 的 `docs.rs` 页面。
- 贡献指南与许可证：详见仓库根目录的 `LICENSE` 与 `CONTRIBUTING`（如存在）。

如果在使用过程中遇到问题，欢迎在 GitHub Issue 区反馈或提交 Pull Request。祝你使用顺利！
