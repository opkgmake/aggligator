# Command-Line Help for `agg-tunnel`

This document contains the help content for the `agg-tunnel` command-line program.

**Command Overview:**

* [`agg-tunnel`↴](#agg-tunnel)
* [`agg-tunnel client`↴](#agg-tunnel-client)
* [`agg-tunnel server`↴](#agg-tunnel-server)
* [`agg-tunnel show-cfg`↴](#agg-tunnel-show-cfg)

## `agg-tunnel`

通过聚合链路的连接转发 TCP 端口。

Aggligator 会将多条 TCP 链路合并为一个逻辑连接， 既汇聚所有链路的带宽，也能在单条链路故障时保持连接稳定。

**Usage:** `agg-tunnel [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `client` — 隧道客户端。
* `server` — 隧道服务器。
* `show-cfg` — 显示默认配置。

###### **Options:**

* `--cfg <CFG>` — 配置文件。
* `-d`, `--dump <DUMP>` — 将分析数据写入文件。



## `agg-tunnel client`

隧道客户端。

**Usage:** `agg-tunnel client [OPTIONS] --port <PORT>`

###### **Options:**

* `-4`, `--ipv4` — 使用 IPv4。
* `-6`, `--ipv6` — 使用 IPv6。
* `-n`, `--no-monitor` — 不显示链路监视器。
* `-a`, `--all-links` — 在监视器中显示所有可能的链路（包括未连接的链路）。
* `-p`, `--port <PORT>` — 指定要从服务器转发到客户端的端口。

   格式为 `server_port:client_port`，可重复指定。

   目标端口必须在服务器端启用。
* `-g`, `--global` — 在所有本地网卡上监听转发端口。

   未指定时仅接受回环接口连接。
* `--once` — 处理完一条连接后立即退出。
* `--ctcp-key <KEY>` — 自定义 CTCP printable 加密密钥（支持十进制、0x 十六进制、0b 二进制或 0o 八进制，默认沿用 openppp2 的内置值）。

  Default value: `154543927`
* `--tcp <TCP>` — TCP 服务器的名称或 IP 地址与端口号。
* `--tcp-link-filter <TCP_LINK_FILTER>` — TCP 链路过滤方式。

   none：不过滤任何链路。

   interface-interface：为每对本地和远端网卡创建一条链路。

   interface-ip：为每个本地网卡与远端 IP 的组合创建一条链路。

  Default value: `interface-interface`
* `--tcp-turbo` — 启用 openppp2 Turbo 风格的 TCP 优化（禁用 Nagle 并放大缓冲区）。
* `--tcp-send-buffer <BYTES>` — 自定义 TCP 发送缓冲区大小（字节，0 表示使用系统默认值）。
* `--tcp-recv-buffer <BYTES>` — 自定义 TCP 接收缓冲区大小（字节，0 表示使用系统默认值）。
* `--tcp-tos <TOS>` — 设置 IPv4 数据包的 TOS/DSCP 值（默认 Turbo 模式下为 0x10）。
* `--usb <USB>` — USB 设备序列号（等同于测速设备的主机名）。

   使用 - 匹配任意设备。
* `--usb-interface-name <USB_INTERFACE_NAME>` — USB 接口名称。

  Default value: `聚合隧道`



## `agg-tunnel server`

隧道服务器。

**Usage:** `agg-tunnel server [OPTIONS] --port <PORT>`

###### **Options:**

* `-n`, `--no-monitor` — 不显示链路监视器。
* `-p`, `--port <PORT>` — 指定要转发给客户端的端口。

   格式为 `port` 或 `target:port`，可重复指定。

   目标可以是主机名或 IP 地址；未指定时默认使用 localhost。
* `--tcp <TCP>` — 要监听的 TCP 端口。
* `--tcp-turbo` — 启用 openppp2 Turbo 风格的 TCP 优化（禁用 Nagle 并放大缓冲区）。
* `--tcp-send-buffer <BYTES>` — 自定义 TCP 发送缓冲区大小（字节，0 表示使用系统默认值）。
* `--tcp-recv-buffer <BYTES>` — 自定义 TCP 接收缓冲区大小（字节，0 表示使用系统默认值）。
* `--tcp-tos <TOS>` — 设置 IPv4 数据包的 TOS/DSCP 值（默认 Turbo 模式下为 0x10）。
* `--ctcp-key <KEY>` — 自定义 CTCP printable 加密密钥（支持十进制、0x 十六进制、0b 二进制或 0o 八进制，默认沿用 openppp2 的内置值）。

  Default value: `154543927`
* `--usb-interface-name <USB_INTERFACE_NAME>` — USB 接口名称。

  Default value: `聚合隧道`



## `agg-tunnel show-cfg`

显示默认配置。

**Usage:** `agg-tunnel show-cfg`



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>

