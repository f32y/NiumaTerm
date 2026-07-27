# remote-host

主进程内的远程会话托管（`crates/app` remote 模块，Host 侧）。

## ADDED Requirements

### Requirement: Host 身份与密钥落盘
Host SHALL 首次启用远程功能时生成静态 X25519 密钥对，私钥以 DPAPI（CryptProtectData）加密存于 `%LOCALAPPDATA%\NiumaTerm\`，`host_id` SHALL 派生为 `hex(SHA-256(host_pubkey))[..16]`。

#### Scenario: 首次启用生成密钥
- **WHEN** 用户首次开启 Host 服务
- **THEN** 生成密钥对并 DPAPI 加密落盘，重启后加载同一身份

#### Scenario: 私钥文件被拷贝到其他用户
- **WHEN** 密钥文件在另一 Windows 用户上下文中被读取
- **THEN** DPAPI 解密失败，身份无法冒用

### Requirement: 设备配对与授权列表
Host SHALL 生成一次性配对码（TTL 5 分钟），并在 XX 握手通道内验证 pairing_token 后将 Client 静态公钥写入授权设备列表；token SHALL 一次性作废。Host SHALL 支持查看与移除授权设备，被移除设备的后续 IK 握手 MUST 被拒绝。

#### Scenario: 配对成功
- **WHEN** Client 在 TTL 内以 XX 握手提交正确 pairing_token
- **THEN** 其公钥持久化到授权列表，token 作废，后续 IK 握手直接放行

#### Scenario: token 过期或重用
- **WHEN** Client 提交已过期或已使用过的 pairing_token
- **THEN** Host 拒绝配对并关闭连接

#### Scenario: 撤销设备
- **WHEN** 用户从授权列表移除某设备
- **THEN** 该设备现有连接被断开，且后续握手被拒

### Requirement: 控制帧到 RemoteSessionHub 的映射
Host SHALL 将控制帧一一映射到 hub API：ListSessions→`list_sessions`、Open→`open`、Attach→`attach`、Kill→`kill`；数据帧 Input→`write_input`、Resize→`resize`；hub 的 `SessionEvent::Output/Exited` SHALL 编码为对应数据帧下发给已 attach 的 Client。hub 错误 SHALL 以 Error 控制帧回传。

#### Scenario: 远程开会话并交互
- **WHEN** 授权 Client 发送 Open 后 Attach
- **THEN** 收到 Attached(snapshot)，随后 Input 帧写入 shell、Output 帧按 seq 顺序下发

#### Scenario: 操作不存在的会话
- **WHEN** Client 对已关闭的 session_id 发送 Attach
- **THEN** 收到携带 SessionNotFound 语义的 Error 帧，连接保持

### Requirement: 会话生命周期独立于连接
Client 断开或订阅队列溢出 SHALL 仅移除该订阅者，SHALL NOT 终止 shell 进程；Host 进程存活期间会话持续运行，重连 Attach 可恢复。

#### Scenario: 掉线不杀会话
- **WHEN** 唯一已 attach 的 Client 断线
- **THEN** shell 继续运行，稍后重连 Attach 得到包含此间输出的新 snapshot

### Requirement: relay 出站连接与重连
Host SHALL 出站建立一条控制 socket（携带 host_id 与 access token）注册到 relay，并响应控制消息：收到 `connected` 时为该 connection_id 开一条数据 socket 并在其上执行 Noise 握手，收到 `disconnected` 时关闭对应数据 socket 与订阅，收到 `sync` 时按全量列表对账补开/关闭数据 socket。控制 socket 断开后 SHALL 以带上限的退避（如 min(30s, 1s×attempt)）自动重连；SHALL 以 WebSocket 协议层 ping 保活，超时视为断开。

#### Scenario: Client 接入触发数据 socket
- **WHEN** 控制 socket 收到某 connection_id 的 `connected`
- **THEN** Host 开对应数据 socket，与该 Client 完成 Noise 握手后进入会话协议

#### Scenario: relay 侧中断恢复
- **WHEN** relay 不可达导致控制 socket 掉线，随后恢复
- **THEN** Host 自动重连注册，收到 `sync` 后为仍在线的 connection_id 重建数据 socket，无需用户干预
