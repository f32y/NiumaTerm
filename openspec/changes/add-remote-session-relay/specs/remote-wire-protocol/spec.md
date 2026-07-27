# remote-wire-protocol

Host↔Client 端到端加密通道与二进制帧格式（`crates/remote_protocol`）。

## ADDED Requirements

### Requirement: Noise 端到端加密通道
系统 SHALL 使用 Noise Protocol（snow crate）在 Host 与 Client 之间建立端到端加密通道：常规连接使用 IK 模式（Client 预知 Host 静态公钥），配对首连使用 XX 模式（互换静态公钥）。握手完成前 SHALL 拒绝一切应用帧。

#### Scenario: IK 握手成功
- **WHEN** Client 持有 Host 静态公钥并发起 IK 握手，且其静态公钥在 Host 授权列表中
- **THEN** 双方进入 transport 阶段，可互发加密帧

#### Scenario: 未授权设备被拒
- **WHEN** IK 握手中 Client 的静态公钥未出现在 Host 授权设备列表
- **THEN** Host 终止握手并关闭连接，无任何会话数据外泄

#### Scenario: 篡改帧被拒
- **WHEN** transport 阶段某加密帧的任意一个字节被中间人修改
- **THEN** 接收方解密失败，丢弃该帧并关闭通道

#### Scenario: 重放帧被拒
- **WHEN** 中间人重发一条此前合法的加密帧
- **THEN** 接收方因 nonce 计数器不匹配而解密失败，丢弃该帧并关闭通道

### Requirement: 二进制帧格式
系统 SHALL 在 Noise 通道内使用二进制帧（每帧对应一条传输消息——一条 WS binary 消息承载一条 Noise 密文，解密即一帧，无需额外长度前缀），首字节为帧类型：`0x00` 控制帧（postcard 序列化的枚举），`0x01` Output（`[session_id u64 LE][seq u64][bytes]`），`0x02` Input（`[session_id][bytes]`），`0x03` Resize（`[session_id][cols u16][rows u16]`），`0x04` Exited（`[session_id][seq u64]`）。编解码 SHALL 为往返无损（roundtrip）。

#### Scenario: 编解码往返
- **WHEN** 任意合法帧被编码后再解码
- **THEN** 得到与原值相等的帧

#### Scenario: 未知帧类型
- **WHEN** 解码器遇到未定义的首字节
- **THEN** 返回错误而非 panic，调用方可选择关闭通道

### Requirement: 配对码编解码
系统 SHALL 提供一次性配对码的编解码：base32 编码的 `{relay_url, host_id, host_static_pubkey, pairing_token(16B)}`，可由用户手抄传递。

#### Scenario: 配对码往返
- **WHEN** Host 生成配对码且 Client 解析该字符串
- **THEN** Client 得到与 Host 生成时一致的 relay_url、host_id、公钥与 token

#### Scenario: 损坏的配对码
- **WHEN** 用户输入被截断或抄错的配对码
- **THEN** 解析返回明确错误，无 panic
