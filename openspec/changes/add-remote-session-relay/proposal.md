# Proposal: add-remote-session-relay

## Why

NiumaTerm 的会话只能在本机使用。`crates/remote_session_hub` 已经实现了可分离的无头会话模型（VT checkpoint + 序列号事件流），但主进程从未接入它，也没有任何网络层。用户需要从其他电脑远程接入家里/公司机器上的终端会话，且双方通常都在 NAT 后面，需要一个公网中转服务器，同时中转服务器必须无法窥探会话内容。

## What Changes

- 新增 `crates/remote_protocol`：共享线协议 crate——Noise IK/XX 端到端加密通道封装（基于 `snow`）、二进制帧编解码（控制帧 + 终端数据帧）、配对码编解码。
- 新增 `relay/`：Cloudflare Worker + Durable Object 中转服务器（TypeScript，wrangler 部署，对照 paseo 的 cloudflare-adapter 移植）。每 host_id 一个 DO 实例，Host 控制 socket + 每 Client 一条数据 socket，按 connection_id 配对的哑字节转发，只见密文；自托管 Rust relay 留作后续可互换的备选实现。
- 新增 `crates/remote_net`：无 UI 依赖的网络引擎（可无头测试）——DPAPI 密钥存取、授权设备列表、Host 服务（relay 控制/数据 socket、控制帧到 `RemoteSessionHub` 的映射、事件泵、配对）、Client 连接器（IK 连接 / XX 配对）。
- `crates/app` 新增 remote UI 层（引擎来自 remote_net）：
  - Host 侧：服务开关、配对码展示、授权设备管理。
  - Client 侧：远程标签页，经 relay 连接远端 Host，复用现有终端渲染管线。
- 新增依赖：`tokio`(full)、`tokio-tungstenite`+`rustls`、`snow`、`postcard`、`rand`、`sha2`。
- `crates/remote_session_hub` 零改动（其文档已声明网络/认证/编码归宿主进程管）。

## Capabilities

### New Capabilities

- `remote-wire-protocol`: Host↔Client 端到端加密通道（Noise IK/XX，重放免疫、前向保密）与二进制帧格式（控制帧 postcard 编码，终端数据帧 opcode 分流）。
- `relay-server`: 公网中转服务器（Cloudflare Durable Object）——双出站 WebSocket 接入、每 host_id 一个 DO 实例、控制/数据双 socket 按 connection_id 配对转发密文、access token 防滥用、hibernation 兼容。
- `remote-host`: 主进程内托管远程会话——设备配对与授权列表、静态密钥 DPAPI 落盘、控制帧到 RemoteSessionHub 的映射、断线后会话存活。
- `remote-client`: 远程标签页——配对码接入、attach 拿 snapshot 后接续事件流、断线重连从新 checkpoint 恢复、输入/resize 上行。

### Modified Capabilities

（无——openspec/specs/ 目前为空，无既有 spec 受影响）

## Impact

- 代码：新增 `crates/remote_protocol`（Rust）与 `relay/`（TypeScript Worker）；`crates/app` 新增 remote 模块与配对/远程标签 UI；根 `Cargo.toml` workspace 成员与依赖更新。
- 依赖：Rust 侧净新增 tokio-tungstenite、rustls、snow、postcard、sha2（tokio/serde/base64 已在锁文件中）；relay 侧引入 wrangler/workers-types 工具链。
- 部署：relay 走 `wrangler deploy` 到 Cloudflare（免费额度即可，TLS 由 CF 边缘提供）；Host/Client 均为出站连接，无端口暴露。
- 安全面：新增用户级密钥文件（`%LOCALAPPDATA%\NiumaTerm\host-key`，DPAPI 加密）与授权设备列表文件。
