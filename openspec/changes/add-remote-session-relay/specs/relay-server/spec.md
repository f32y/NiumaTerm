# relay-server

公网中转服务器：Cloudflare Worker + Durable Object（TypeScript，仓库 `relay/` 目录）。每个 `host_id` 一个 DO 实例（`idFromName(host_id)`），采用 WebSocket hibernation 降低成本。连接模型沿用 paseo v2：Host 控制 socket + 每个 Client 连接一条 Host 数据 socket，使 relay 对内层帧保持完全不透明。

## ADDED Requirements

### Requirement: 按 host_id 路由到 Durable Object 实例
Worker SHALL 将 `wss://<relay>/ws?host_id=<id>&role=<host|client>[&connection_id=<cid>]` 的升级请求路由到 `idFromName(host_id)` 对应的 DO 实例；同一 host_id 的全部 socket MUST 落在同一实例。

#### Scenario: 同 host_id 汇聚
- **WHEN** Host 与多个 Client 携带同一 host_id 连接
- **THEN** 它们由同一 DO 实例处理，Client 帧可达该 Host

#### Scenario: 缺失必要参数
- **WHEN** 升级请求缺少 host_id 或 role 非法
- **THEN** Worker 拒绝升级并返回明确错误码

### Requirement: Host 控制 socket 注册
DO SHALL 接受 Host 的控制 socket（`role=host`，无 connection_id），每个 host_id 至多一条；重复注册 SHALL 以新连接替换旧连接并关闭旧 socket。注册 MUST 携带有效 access token（Worker secret 配置），无效 token 的连接 SHALL 被立即关闭且不建立注册。DO SHALL 通过控制 socket 向 Host 推送 JSON 控制消息：`connected`/`disconnected`（单个 connection_id 上线/下线）与 `sync`（当前在线 connection_id 全量列表，用于重连后对账）。

#### Scenario: 注册成功并接收通知
- **WHEN** Host 携带有效 token 建立控制 socket，随后一个 Client 接入
- **THEN** Host 在控制 socket 上收到含该 connection_id 的 `connected` 消息

#### Scenario: 无效 token
- **WHEN** 控制 socket 升级请求的 token 缺失或错误
- **THEN** DO 关闭连接且不注册

#### Scenario: 控制 socket 重连对账
- **WHEN** Host 控制 socket 断开重连时仍有 Client 在线
- **THEN** Host 收到 `sync` 消息，据此为每个在线 connection_id 补开数据 socket

### Requirement: Client 接入与双 socket 配对转发
DO SHALL 为 Client socket（`role=client`）分配 `conn_<uuid>` 形式的 connection_id（请求未携带时），并在该 Client socket 与 Host 侧同 connection_id 的数据 socket（`role=host&connection_id=<cid>`）之间双向转发。转发 MUST 视帧为不透明字节，SHALL NOT 解析、修改或持久化内层内容。

#### Scenario: 双向转发
- **WHEN** Client 与对应 Host 数据 socket 均已就绪
- **THEN** 任一方发送的帧原样到达另一方

#### Scenario: 目标 Host 不在线
- **WHEN** Client 连接的 host_id 无已注册控制 socket
- **THEN** DO 关闭该 Client 连接并返回明确关闭码

### Requirement: 数据 socket 就绪前的帧缓冲
Host 数据 socket 尚未接入时，DO SHALL 缓冲该 connection_id 的 Client 帧（上限 200 帧），数据 socket 接入后按序冲刷；超过上限 SHALL 关闭该 Client 连接迫使其重连。

#### Scenario: 缓冲后冲刷
- **WHEN** Client 在 Host 数据 socket 就绪前发送了若干帧（未超上限）
- **THEN** 数据 socket 接入后按原顺序收到全部缓冲帧

#### Scenario: 缓冲溢出
- **WHEN** 缓冲帧数超过 200
- **THEN** DO 关闭该 Client 连接，丢弃缓冲

### Requirement: 断开级联
Host 控制 socket 断开时 DO SHALL 关闭该 host_id 下全部 Client socket 与数据 socket（触发 Client 重连）；单个 Client 断开 SHALL 关闭其配对的数据 socket 并向控制 socket 推送 `disconnected`，不影响其他 Client。

#### Scenario: Host 掉线
- **WHEN** Host 控制 socket 断开
- **THEN** 该 host_id 的所有 Client 连接被关闭，注册清除

#### Scenario: 单 Client 掉线
- **WHEN** 某 Client socket 断开
- **THEN** 对应数据 socket 被关闭，Host 收到 `disconnected`，其余 Client 不受影响

### Requirement: Hibernation 兼容
DO SHALL 使用 WebSocket hibernation API（`acceptWebSocket` + `serializeAttachment`）承载全部 socket：实例休眠再唤醒后，socket 的角色与 connection_id 归属 MUST 可恢复，转发不中断。

#### Scenario: 休眠后唤醒
- **WHEN** DO 因空闲休眠后某 socket 收到新帧
- **THEN** DO 从 attachment 恢复路由归属并正常转发
