# Tasks: add-remote-session-relay

## 1. remote_protocol：协议与加密通道

- [x] 1.1 创建 `crates/remote_protocol`，加入 workspace；依赖 snow、postcard、serde、rand、sha2、base32
- [x] 1.2 定义 wire 镜像类型（SessionOptions'/SessionInfo'/SessionSnapshot'）与控制帧枚举，postcard 编解码 + roundtrip 测试
- [x] 1.3 实现数据帧编解码（0x01 Output / 0x02 Input / 0x03 Resize / 0x04 Exited，帧与传输消息一一对应 + 首字节分流），未知帧类型返回错误，roundtrip 测试
- [x] 1.4 封装 Noise 通道：IK 与 XX 握手、transport 加解密、握手期 remote_static 暴露（供 Host 校验授权列表）
- [x] 1.5 loopback 安全测试：篡改一字节必失败、重放帧必失败、未授权 client 公钥 IK 握手必拒
- [x] 1.6 配对码编解码（base32 的 {relay_url, host_id, host_pubkey, pairing_token}），含损坏输入错误处理测试

## 2. relay：Cloudflare Durable Object 中转服务器

- [x] 2.1 创建 `relay/` Worker 项目（TypeScript + wrangler + workers-types）；Worker 入口按 `host_id` 路由升级请求到 `idFromName(host_id)` 的 DO 实例，缺参/非法 role 拒绝
- [x] 2.2 Host 控制 socket：access token（Worker secret）校验、重复注册替换旧连接、`connected`/`disconnected`/`sync` 控制消息推送
- [x] 2.3 Client socket 与 Host 数据 socket：connection_id 分配（`conn_<uuid>`）、按 connection_id 配对双向不透明转发、目标 Host 不在线时明确关闭码（升级阶段 404/409/401 拒绝）
- [x] 2.4 数据 socket 就绪前帧缓冲（上限 200，溢出断连 4429）；断开级联（Host 控制 socket 掉线关其全部 Client；单 Client 掉线关数据 socket 并推 `disconnected`）
- [x] 2.5 Hibernation 兼容：全部 socket 走 `acceptWebSocket` + `serializeAttachment` 存路由归属，路由查找全部经 tag 查询（无内存态依赖；帧缓冲按设计接受休眠丢弃）
- [x] 2.6 集成测试（`wrangler dev` 本地 DO）：Rust 双端经 relay 完成 Noise 握手（含缓冲冲刷）+ 加密 echo 往返；无效/缺失 token 401；host 离线 404；缓冲溢出 4429 断连

## 3. Host 侧：主进程托管

- [x] 3.1 新建 `crates/remote_net` 网络引擎 crate（无 GPUI 依赖、可无头测试；app 后续只加 UI）：HostHandle::start 起独立 tokio runtime 线程；Host 静态密钥生成 + DPAPI 落盘/加载，host_id 派生
- [x] 3.2 relay 控制 socket：出站注册 + WS ping 保活 + 断线退避重连（min(30s, 1s×attempt)）；响应 `connected`/`disconnected`/`sync`，按需开关每 connection_id 的数据 socket
- [x] 3.3 控制帧 ↔ RemoteSessionHub 映射：ListSessions/Open/Attach/Detach/Kill/Error 回传；Input/Resize 数据帧到 hub；首消息模式字节区分 IK/配对
- [x] 3.4 Output/Exited 事件泵：每订阅一个 std 桥线程转发到 tokio 通道，Output 按 MAX_DATA_LEN 分块；订阅溢出/断线只移除订阅者，shell 存活
- [x] 3.5 配对流程：一次性配对码生成（TTL 5 分钟，take() 一次性作废）、XX 握手内 token 校验、authorized_devices.json 持久化；revoke_device 断开在线连接
- [x] 3.6 端到端测试（host_e2e，经 wrangler dev relay）：配对 → 开 cmd.exe → echo marker 加密往返 → 断线重连新 snapshot 含此间输出；未授权 IK 与伪 token 被拒

## 4. Client 侧：连接运行时（引擎）+ 远程标签页（UI）

- [x] 4.1a 引擎：Client 设备密钥生成 + DPAPI 落盘（复用 keys.rs）、XX 配对连接器（client_connect_pair，含 host 公钥 pin 校验）
- [x] 4.1b 引擎：`open_remote_session`/`list_remote_sessions` 同步运行时——独立线程跑 tokio，暴露 `RemoteSession{output: std mpsc<SessionByteEvent>, send_input, send_resize}`，正是 NetPty 需要的形状；e2e 验证字节流往返与会话列表
- [x] 4.2 UI：客户端配对（Settings 里输入配对码 pair_with_code）+ 已配对主机列表/Forget；连接经后台 task 不阻塞 UI，失败弹通知
- [x] 4.3 UI：`NetPty: EventedPty`（SoftReady 就绪信号，reader 先喂 snapshot.vt 再喂 Output / writer→send_input / set_winsize→send_resize / 通道关闭→Exited）接入 PtyPipe 泛型 seam；TerminalSession::new_remote + surface/pane spawn_remote 复用现有渲染/wake/pump 链路；NetReader 单测 + 经真实 relay 的 render e2e（remote_session_renders_through_net_pty）
- [~] 4.4 UI：`NewRemoteTab`（Ctrl+Shift+R）连接首个已配对主机开远程标签页。会话列表选择页（list_remote_sessions + AttachTarget::Existing 多主机picker）留待后续：先连首个主机，用户真需要多主机再加 picker

## 5. 配对管理与收尾

- [ ] 5.1 Host 授权设备管理 UI：列表、移除（断开现有连接 + 后续握手拒绝）、新设备接入通知
- [ ] 5.2 Host 服务开关 UI 与配对码展示
- [x] 5.3 relay 部署文档（relay/README.md：`wrangler deploy` + `wrangler secret put ACCESS_TOKEN` + 自定义域名绑定 + 本地开发/协议表）
- [ ] 5.4 全量验证：`cargo test` 全绿；`--testing` 启动双实例手动走通配对→远程会话→撤销设备
