# Design: add-remote-session-relay

## Context

`crates/remote_session_hub` 提供了完整的可分离会话模型：`RemoteSessionHub::attach()` 返回 VT checkpoint（`SessionSnapshot { base_seq, vt, cols, rows }`）加带序列号的 `SessionEvent` 流；订阅队列溢出即静默掉线，重连从新 checkpoint 开始。该 crate 的文档明确声明网络、认证、线编码归宿主进程负责，目前主进程尚未接入它。

仓库现状：无任何网络/加密依赖（`rustls`/`tungstenite`/`snow`/`quinn` 均不在 Cargo.lock）；`tokio`/`mio`/`smol` 以最小 feature 形态由 vendored GPUI 传递引入；`serde`/`base64` 可用。Windows-only。

参考实现为 paseo（自托管 AI agent 编排器）：daemon 与客户端均出站连接不可信 relay，端到端 NaCl 加密，配对靠 QR 带外分发公钥。其已知弱点：无重放保护（随机 nonce 无计数器）、客户端无持久身份、授权全有全无。本设计针对性修掉前两点。

## Goals / Non-Goals

**Goals:**

- 多台 NiumaTerm 客户端经公网中转服务器接入一台 Host 的终端会话。
- Host 与 Client 均为出站连接，零端口暴露、穿 NAT。
- 中转服务器不可信：只见密文与路由元数据，重放/篡改帧对端必然拒收。
- 设备级身份：每台客户端有静态密钥，Host 维护授权列表，可单独撤销。
- 断线重连语义直接复用 hub 的 checkpoint 模型，协议无 ack/重传窗口。

**Non-Goals:**

- 会话内细粒度权限（只读观看、按会话授权）——授权设备即完全操作员，与 paseo 一致。
- 局域网直连模式的 UI 入口（Noise 层天然支持直跑 TCP，本期只做 relay 路径）。
- 跨平台（Host 依赖 ConPTY，Windows-only；Client 侧协议无平台假设但本期不验证）。
- relay 的水平扩展/多实例（单实例内存路由表即可）。
- 文件传输、端口转发等终端外功能。

## Decisions

### D1: 传输拓扑——双出站 WebSocket 经中转

Host 与 Client 各自出站 WSS 连到 relay，relay 按 `host_id` 配对后双向转发字节。

- 备选 QUIC/quinn：省一层 TLS 握手，但生态成熟度、代理穿透性、调试工具都差于 WebSocket；paseo 用 WS 验证了该形态。
- 备选 Host 直接监听端口：要求用户配防火墙/公网 IP，违背零暴露目标。

### D2: 端到端加密——Noise Protocol（`snow` crate），弃用 paseo 的裸 NaCl box

- Noise IK（常规连接）/ XX（配对首连）：握手内完成双向身份认证 + 前向保密（ephemeral key）。
- Transport 阶段每方向递增 nonce 计数器 → relay 重放帧解密必败，直接修掉 paseo 的重放漏洞。
- 备选 mTLS：证书管理对个人用户过重；Noise 密钥即身份，配对码即信任分发。
- 备选手搓 X25519+AEAD：等于重新发明 Noise 且容易踩 nonce 复用坑。

### D3: 身份与配对

- Host 静态 X25519 密钥对即 Host 身份，`host_id = hex(SHA-256(host_pubkey))[..16]` 作 relay 路由键。
- Client 也持静态密钥对（设备身份）。IK 握手中 Host 校验 client 静态公钥 ∈ `authorized_devices.json`，删一行即撤销该设备。
- 首次配对：Host 生成一次性配对码（TTL 5 分钟，base32 编码 `{relay_url, host_id, host_pubkey, pairing_token(16B)}`）→ 用户带外抄到客户端 → Client 用 XX 握手（互换静态公钥）→ 加密通道内提交 pairing_token → Host 验证后持久化 client 公钥、作废 token。
- 备选共享密码（paseo 直连模式）：无设备概念，泄露即全员换密码，撤销粒度为零。

### D4: 线格式——Noise 通道内长度前缀二进制帧，首字节分流

```
控制帧 0x00: postcard 序列化枚举
  C→H: ListSessions | Open(SessionOptions') | Attach(id) | Detach | Kill(id)
  H→C: SessionList(Vec<SessionInfo'>) | Attached(SessionSnapshot') | Error(...)
数据帧: [opcode u8][session_id u64 LE][payload]
  0x01 Output  payload=[seq u64][bytes]   ← SessionEvent::Output
  0x02 Input   payload=bytes              → hub.write_input()
  0x03 Resize  payload=[cols u16][rows u16]
  0x04 Exited  payload=[seq u64]
```

- postcard 而非 JSON：紧凑、no_std 友好、serde 生态；数据帧裸编码避免终端输出被序列化包裹（照抄 paseo 的 demux 思路）。
- 帧内类型是 wire 专用镜像类型（带 `'`），与 hub 类型解耦，hub 保持零改动。

### D5: relay 优先实现为 Cloudflare Durable Object（paseo 方案）

- TypeScript Worker + DO，仓库 `relay/` 目录，wrangler 部署。每个 `host_id` 一个 DO 实例（`idFromName`），WebSocket hibernation 降低空闲成本。
- 选它的理由：零运维（无 VPS、无 systemd、无证书——CF 边缘自动 TLS）、免费额度覆盖个人场景、全球边缘就近接入；paseo 已在生产验证该形态，`packages/relay/src/cloudflare-adapter.ts` 可直接对照移植。
- 连接模型照抄 paseo v2：Host 一条**控制 socket**（注册 + 接收 `connected`/`disconnected`/`sync` 通知）+ 每个 Client 连接一条 **Host 数据 socket**，Client socket 与数据 socket 按 connection_id 配对。这样 DO 对内层帧保持字节级不透明——单连接多路复用则要求 relay 解析外层信封，放弃。
- 数据 socket 就绪前 DO 缓冲 Client 帧（上限 200，溢出断连迫使重连）。
- 注册需 access token（Worker secret），防公网滥用/host_id 占坑。占坑者在 E2EE 下只能造成拒绝服务，签名挑战留作升级路径。
- 备选自托管 Rust bin（tokio + tungstenite + DashMap 路由表）：语言栈统一、无厂商绑定，留作升级路径——relay 协议是哑字节转发，两种实现可互换，Host/Client 侧代码零改动。

### D6: 密钥落盘——DPAPI

Host/Client 静态私钥存 `%LOCALAPPDATA%\NiumaTerm\`，`CryptProtectData` 加密（`windows` crate 已在依赖），绑定当前 Windows 用户。备选明文 + ACL：DPAPI 成本几行代码，防离线拷贝，无理由不用。

### D7: 断线重连 = 重新握手 + Attach

Client 重连后重新 Noise 握手、发 `Attach`，拿全新 snapshot 渲染。无恢复令牌、无事件回放缓冲——hub 的 checkpoint 语义已保证"每字节要么在 snapshot 里要么在其后事件里"。Host 侧会话生命周期独立于连接，掉线不杀 shell。

### D8: Host 侧异步运行时

网络引擎放在独立的 `crates/remote_net`（无 GPUI 依赖，app 只加 UI），`HostHandle::start` 在主进程内起独立 tokio runtime 线程跑网络栈；hub 的每个 `SessionSubscription` 由一个 std 桥线程转发进 tokio 通道。避免向 GPUI 的 smol 生态里塞 tungstenite 适配层，同时让整条 Host 链路可无头端到端测试（tests/host_e2e.rs）。

## Risks / Trade-offs

- [snow/Noise 接线错误导致安全形同虚设] → remote_protocol 首先落地，loopback 测试强制覆盖"篡改一字节必失败、重放帧必失败、未授权 client 公钥握手必拒"。
- [relay 单点故障] → Host/Client 均带指数退避重连；relay 无状态，重启即恢复；会话在 Host 存活，断连只丢在线性。
- [access token 泄露 → host_id 占坑 DoS] → 已知天花板，E2EE 保证机密性不受影响；升级路径为注册时对 host_id 做密钥签名挑战。
- [Cloudflare 厂商绑定 / DO 计费或政策变化] → relay 协议为哑字节转发，自托管 Rust 实现可无缝替换（D5 备选），Host/Client 零改动。
- [Hibernation 语义踩坑（休眠丢内存态）] → 全部路由归属存 `serializeAttachment`，帧缓冲落 DO storage 或接受休眠时丢弃缓冲（缓冲仅存在于数据 socket 未就绪的短窗口）；集成测试覆盖休眠唤醒路径。
- [配对码被截获（5 分钟窗口内）] → 截获者可完成配对成为授权设备；缓解：TTL 短、一次性、Host UI 显示新设备接入通知，授权列表可随时撤销。
- [双会话模型并存（app 的 TerminalSession 与 hub 的 RemoteSession 近似重复）] → 本期接受重复，避免动本地终端路径；后续可让本地标签也走 hub 收敛。
- [tokio full features 引入编译时间/体积] → 可接受；仅 app 与 relay_server 引用。

## Migration Plan

纯新增，无既有行为变更。分五步落地，每步独立可验（见 tasks.md）；relay 用 wrangler 部署到 Cloudflare（`wrangler deploy` + `wrangler secret put ACCESS_TOKEN`），TLS 由 CF 边缘自动提供，本地开发用 `wrangler dev` 起本地 DO 跑集成测试。回滚 = 不启用 Host 服务开关，新增代码不影响本地终端路径。

## Open Questions

- 配对码承载形式：纯短码（手抄）够用，QR 是否本期做（需要二维码渲染依赖）？倾向本期只做文本码。
- 帧缓冲的休眠持久性：随 hibernation 丢弃（简单，窗口极短）还是写 DO storage（不丢帧但多一次 IO）？倾向丢弃 + Client 重连兜底。
- `SessionOptions'` 暴露多少字段给远程 Client（shell/cwd/env 任意指定意味着授权设备可执行任意命令——与"授权即完全操作员"模型一致，但 UI 上是否默认只允许开默认 shell）？
