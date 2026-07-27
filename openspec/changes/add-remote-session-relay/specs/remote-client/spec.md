# remote-client

远程标签页（`crates/app` remote 模块，Client 侧）。

## ADDED Requirements

### Requirement: 配对码接入
Client SHALL 接受用户粘贴/输入的配对码，解析出 relay_url、host_id、Host 公钥与 pairing_token，完成 XX 配对握手后持久化 Host 信息与本机设备密钥，后续连接 SHALL 使用 IK 握手且无需再次配对。

#### Scenario: 首次配对
- **WHEN** 用户输入有效配对码
- **THEN** Client 生成/加载本机设备密钥，完成配对，Host 出现在已知主机列表

#### Scenario: 再次连接
- **WHEN** 用户对已配对 Host 发起连接
- **THEN** Client 直接 IK 握手成功，无配对码输入步骤

### Requirement: 远程标签页渲染与输入
Client SHALL 以标签页承载远程会话：Attach 后先以 snapshot 的 VT 状态初始化终端渲染，再按 seq 顺序应用 Output 帧；键盘输入 SHALL 编码为 Input 帧上行，标签页尺寸变化 SHALL 发送 Resize 帧。渲染 SHALL 复用现有终端渲染管线。

#### Scenario: attach 渲染
- **WHEN** Client Attach 成功
- **THEN** 标签页立即显示 snapshot 内容，后续输出增量到达

#### Scenario: 输入与 resize
- **WHEN** 用户在远程标签页键入或调整窗格大小
- **THEN** 远端 shell 收到输入/新尺寸，回显经 Output 帧返回

### Requirement: 断线重连
连接断开后 Client SHALL 以带上限的退避自动重连，重连成功后重新握手并 Attach 原 session_id，以新 snapshot 整体重建终端状态；重连期间标签页 SHALL 显示断线状态。

#### Scenario: 网络闪断
- **WHEN** Client 与 relay 的连接中断数秒后恢复
- **THEN** 标签页短暂显示重连中，随后以新 snapshot 恢复显示，无内容错乱

#### Scenario: 会话已在远端结束
- **WHEN** 重连后 Attach 的 session_id 已退出
- **THEN** 标签页显示会话已结束，允许用户关闭或新开会话

### Requirement: 远端会话列表
Client SHALL 在连接后可请求 SessionList 展示远端会话（标题、shell、是否退出、attach 数），供用户选择 Attach 既有会话或新开会话。

#### Scenario: 列出并接入既有会话
- **WHEN** Host 上已有运行中的会话且 Client 请求列表
- **THEN** 列表显示该会话，用户选择后 Attach 成功
