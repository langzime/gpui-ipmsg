# gpui-ipmsg 软件架构文档

> 版本：0.1.0 ｜ 分析基线：2026-09-04 工作区代码
> 定位：局域网 IP Messenger（飞鸽传书）协议的 Rust 桌面客户端，GPUI 渲染 + Tokio 网络栈

---

## 1. 系统概述

### 1.1 功能范围

| 功能 | 实现状态 |
|---|---|
| UDP 广播上线/下线发现（BR_ENTRY / ANSENTRY / BR_EXIT） | ✅ |
| 单播文本消息（SENDMSG + SENDCHECKOPT） | ✅ |
| 文件/文件夹发送与接收（TCP 2425，GETFILEDATA / GETDIRFILES） | ✅ |
| 聊天历史持久化（JSON） | ✅ |
| 未读计数、多会话切换 | ✅ |
| 用户名/群组/编码/UI 语言设置 | ✅ |
| 消息加密（协议支持 RSA/AES） | ❌ 收到加密包仅显示占位文本 |
| 下线广播（BR_EXIT） | ❌ `send_exit` 已定义但从未调用 |
| 送达确认跟踪（RECVMSG 应答） | ❌ 对端 ACK 被当作 Unknown 事件丢弃 |
| 缺席模式、广播消息、消息重发 | ❌ |

### 1.2 技术栈

- **UI**：GPUI（zed-industries/zed git 依赖）+ gpui-component
- **网络/异步**：Tokio（multi-thread runtime，独立 OS 线程承载）
- **协议**：IPMsg v1（UDP 2425 控制面 + TCP 2425 数据面），编码 GB18030/UTF-8（encoding_rs）
- **持久化**：TOML（config.toml）+ JSON（history.json）
- **i18n**：rust-i18n（zh-CN / en）

---

## 2. 总体架构

### 2.1 分层视图（现状）

```
┌────────────────────────────────────────────────────────────┐
│ 表现层 (GPUI 主线程)                                        │
│  main.rs → ChatShell (chat_shell.rs)                        │
│    ├ sidebar.rs        (侧栏 + 设置窗口 SettingsWindowView) │
│    ├ conversation_list.rs / chat_area.rs / input_area.rs    │
│  轮询: 每 250ms 读 state_seq → 全量拉取状态 → 重建视图模型   │
└──────────────┬─────────────────────────────────────────────┘
               │ 函数调用 logic::*（spawn 异步动作）/ dispatch_cmd
┌──────────────▼─────────────────────────────────────────────┐
│ 应用服务层  logic.rs                                        │
│  - Tokio runtime 引导（独立线程 + pending() 保活）          │
│  - send_text / send_files / download_* / cancel_*           │
│  - ACTIVE_DOWNLOADS 任务表 / open_in_folder / save_settings │
└──────────────┬─────────────────────────────────────────────┘
               │ StateCmd (mpsc 1024)
┌──────────────▼─────────────────────────────────────────────┐
│ 状态层  app_state.rs  "单 actor"                            │
│  run_state_manager: 消费 StateCmd → 变更 CoreState          │
│  → 全量克隆到全局 STATE_SNAPSHOT(Arc<Mutex>)                │
│  → STATE_SEQ.fetch_add(1)  → 全量重写 history.json          │
│  读接口: list_* / get_self_addr_info / state_seq (全局锁)   │
└──────────────┬─────────────────────────────────────────────┘
               │ Event (tokio broadcast, 容量 64)
┌──────────────▼─────────────────────────────────────────────┐
│ 协议层  ipmsg_core/                                         │
│  mod.rs: UDP 收发循环 / TCP 文件服务 / FILE_TABLE 等全局态   │
│  protocol.rs: 协议常量                                      │
└────────────────────────────────────────────────────────────┘
```

### 2.2 模块职责与依赖

| 模块 | 行数级职责 | 依赖方向 |
|---|---|---|
| `main.rs` | 入口，窗口创建，i18n 装载 | → chat_shell, config |
| `chat_shell.rs` | 主视图 + 视图模型构建 + 轮询同步 + 输入事件 | → app_state, logic, config |
| `chat_area.rs` / `conversation_list.rs` / `input_area.rs` / `sidebar.rs` | 均为 `impl ChatShell` 的渲染切片 | → chat_shell, logic |
| `logic.rs` | UI 与异步世界的门面；runtime 生命周期；下载任务登记 | → app_state, ipmsg_core, config |
| `app_state.rs` | 状态 actor、快照发布、历史持久化、事件→消息映射（含 i18n！） | → ipmsg_core, config |
| `ipmsg_core/mod.rs` | 协议编解码、UDP/TCP 服务、文件表、取消令牌、网卡探测 | 仅依赖外部 crate |
| `config.rs` | 配置读写 + 目录约定 + 旧版路径迁移 | — |

依赖方向总体单向（UI → logic → app_state → ipmsg_core），这一点是合格的；但**反向耦合**通过全局静态大量存在（见 §5）。

### 2.3 关键数据流

**收消息**：
```
UDP recv → parse_packet → Event::Message (broadcast)
  → init_state 泵: ApplyEvent → run_state_manager
  → messages.push + unread+1 → 全量克隆进 STATE_SNAPSHOT → 全量重写 history.json
  → STATE_SEQ++
  → ChatShell 250ms 轮询感知 seq 变化 → list_messages() 全量克隆 → 重建会话映射 → render
```

**发消息**：
```
render 输入框 → send_current_message → logic::send_text
  → runtime.spawn(send_message UDP) → 成功才 PushOutgoing
  → run_state_manager → 快照/持久化 → （轮询）→ UI
  失败路径: 无任何 UI 反馈，消息静默消失
```

**收文件**：
```
Event::FileOffer → 快照 → 轮询渲染 → 用户点接收 → rfd 选路径
  → logic::download_file → recv_file (TCP) → 100ms 节流 UpdateProgress
  → actor 反向扫描 messages 定位消息打补丁 → 快照 → （轮询）→ 进度条
```

### 2.4 并发模型清单

| 线程/任务 | 职责 | 同步原语 |
|---|---|---|
| GPUI 主线程 | 渲染 + 输入 + 250ms 轮询任务 | `STATE_SNAPSHOT::Mutex`（读）、`ACTIVE_DOWNLOADS::Mutex` |
| runtime 线程（logic::ensure_started） | Tokio multi-thread，`pending::<()>()` 永久保活 | `RUNTIME_HANDLE::OnceCell`、`STARTED::OnceCell` |
| run_state_manager 任务 | 唯一状态写者（单写者模型，好） | `STATE_SNAPSHOT::Mutex`（写）、`STATE_SEQ::AtomicU64` |
| UDP 收包任务 | 协议分发 | `MAIN_SOCKET::Mutex`、`TEXT_ENCODING::RwLock`、`USER_INFO::RwLock` |
| TCP accept + 每连接任务 | 文件服务 | `FILE_TABLE::Mutex`、`SEND_CANCEL_FLAGS::Mutex` |
| dispatch 兜底线程（try_send 满时） | 阻塞投递 | — |

全局静态共 **13 处**：`STATE_SNAPSHOT`、`STATE_SEQ`、`STATE_CMD_TX`、`RUNTIME_HANDLE`、`STARTED`、`ACTIVE_DOWNLOADS`、`SETTINGS_WINDOW_HANDLE`、`MAIN_SOCKET`、`NET_CONFIG`、`USER_INFO`、`TEXT_ENCODING`、`FILE_TABLE`/`FILE_ID_SEQ`/`PACKET_NO_SEQ`、`SEND_CANCEL_FLAGS`、`EXT_ID_PART`。

### 2.5 持久化

- **config.toml**：`%APPDATA%/gpui-ipmsg/`，支持旧版当前目录迁移（load 时发现旧文件即写回新路径——**load 带副作用**）。
- **history.json**：全部消息单文件数组；每条消息/每次上下线事件触发**整文件重序列化 + 同步写盘**（std::fs 阻塞 IO 直接在 async 任务中执行，[app_state.rs](../src/app_state.rs#L499-L505)）。无容量上限、无轮转、无增量追加。

---

## 3. 架构问题分析

评级：**P0** = 正确性/安全缺陷；**P1** = 结构性缺陷（将随规模放大）；**P2** = 健壮性/协议缺陷；**P3** = 工程质量。

### P0-1 broadcast 通道 Lagged 会永久杀死事件泵

[ipmsg_core/mod.rs](../src/ipmsg_core/mod.rs#L314) 事件通道容量仅 64；[app_state.rs:435](../src/app_state.rs#L435)：

```rust
while let Ok(ev) = ipmsg_rx.recv().await {
```

tokio broadcast 在消费者落后时返回 `Err(RecvError::Lagged)`，`while let Ok` 直接**退出循环**——此后所有网络事件（消息、上线、文件）永久丢失，且无任何日志与 UI 提示。一次群发风暴即可触发。这是整个数据链路的单点失效。

### P0-2 接收文件夹存在路径穿越（任意文件写入）

[ipmsg_core/mod.rs:1362-1385](../src/ipmsg_core/mod.rs#L1362-L1385) 中 `filename` 直接取自网络报文头，未经任何过滤就 `current_dir.join(filename)`。恶意发送方发送文件名为 `..\..\evil.exe` 或绝对路径的目录头，接收端即在其选择的父目录之外写文件。等价于 zip-slip，必须对 filename 过滤路径分隔符、`..` 与盘符。

### P0-3 TCP 文件服务无鉴权，且 ID 可枚举

[handle_tcp_file](../src/ipmsg_core/mod.rs#L1555) 对任何连入 2425 的对端，只要报文中的 `(packet_no, file_id)` 命中 `FILE_TABLE` 就发送文件内容，**不校验请求者是否为原定接收者**。而 `FILE_ID_SEQ` 从 1 顺序递增、`packet_no` 为顺序计数器，两者均可内网穷举。局域网内任意主机可拉取他人正在传输的文件。`IPMSG_NET_PREFIX` 前缀过滤是唯一屏障，且为字符串 `starts_with` 实现，脆弱。

### P0-4 `Drop for ChatShell { std::process::exit(0) }`

[chat_shell.rs:441-445](../src/chat_shell.rs#L441-L445) 在视图 Drop 中直接杀进程：绕过所有析构、不发送 BR_EXIT 下线广播（对端将长期显示本机在线）、不落盘任何未持久化状态。用 Drop 做进程退出语义本身就是对生命周期的误用。

### P0-5 发送失败静默丢弃，用户消息"消失"

[logic.rs:53-67](../src/logic.rs#L53-L67)：`send_message` 失败时既不 PushOutgoing 也不报错，用户输入的消息在 UI 上无任何痕迹。消息可靠性协议（SENDCHECKOPT 要求对端回 RECVMSG）虽有发出，但对端 ACK 在 [ipmsg_core/mod.rs:444-453](../src/ipmsg_core/mod.rs#L444-L453) 被映射为 `Event::Unknown` 后丢弃——**没有送达状态机**。

### P1-1 轮询式 UI 同步：250ms 全量快照 + 全量重建

[chat_shell.rs:156-176](../src/chat_shell.rs#L156-L176) 每 250ms：
1. 磁盘读取 config.toml（探测 UI 语言变化！`load_config` 还可能触发写盘）；
2. seq 变化时 `list_messages()` 克隆**全部**历史消息（内存随时间无界增长）；
3. `refresh_from_state` 从零重建会话表与按会话分组映射。

GPUI 本身提供 `Entity` + `EventEmitter` 推送机制，此处却退化为定时报表。空闲时也在克隆；历史 1 万条消息时每秒 4 次万级深拷贝 + 线性扫描。**这是当前架构最核心的性能与扩展性短板。**

### P1-2 全局静态到处都是，无依赖注入

13 个全局可变单例横跨三层。后果：
- 初始化顺序隐式（`STATE_CMD_TX` 未初始化时 UI 一旦 dispatch 即 `expect` panic，[app_state.rs:103-105](../src/app_state.rs#L103-L105)）；
- `logic::ensure_started` 未完成前所有 send/download 静默 no-op（`RUNTIME_HANDLE` 为 None 直接 return）；
- 不可测试：无法在不触碰全局 socket/文件系统的前提下单测状态层；
- 并发策略不统一（Mutex/RwLock/atomic 随手选）。

### P1-3 状态层混入表现层职责（i18n 在写状态时定格）

[app_state.rs:212](../src/app_state.rs#L212)、[247](../src/app_state.rs#L247) 在**状态机内部**调用 `t!("time.now")`、`t!("file.folder_prefix")`：
- 消息 `time` 字段存的是"刚刚"这类本地化字符串，**不是时间戳**——历史记录永久丢失真实时间，且换语言后新旧消息语言混杂；
- 文件夹前缀文案在 `app_state`（Event::FileOffer）、`chat_shell`（refresh）、`logic`（send_folder）三处重复生成；
- 更严重者：[ipmsg_core/mod.rs:1128](../src/ipmsg_core/mod.rs#L1128) 在**协议层**硬编码中文 `"[文件夹] {}"` 拼进网络报文——UI 文案泄漏到了线上协议。

### P1-4 消息三套表示 + 逐字段手工转换

`app_state::ChatMessage/FileInfo` → `chat_shell::ChatMessage/FileTransfer` → 渲染闭包再拆字段。每次全量转换 O(N) 手写映射，字段增删需三处同步，已经出现 `time` 语义漂移。缺一个统一的领域模型 + 单向映射边界。

### P1-5 会话身份是"字符串化的 SocketAddr"

`conv.id = addr.to_string()`，发送时再 `conv.id.parse::<SocketAddr>()`（[chat_shell.rs:220](../src/chat_shell.rs#L220)）。IPMsg 扩展字段本提供稳定 `UN:` 用户 ID，却未建模。IP 变化即"新人"，历史断裂；身份、消息、未读全部锚在易变地址上。`normalize_addr` 强改端口的补丁进一步说明地址不是可靠身份。

### P1-6 `StateCmd::UpdateProgress` 是 10 个 Option 字段的"上帝命令"

[app_state.rs:81-95](../src/app_state.rs#L81-L95) 用 `Option<bool>` 表达"是否修改该字段"，本质是把部分更新协议做成了 RPC 补丁包。且每次进度回调（100ms 节流）触发 actor **倒序全量扫描 messages** 定位目标（[app_state.rs:342-380](../src/app_state.rs#L342-L380)），历史越大传输越慢。应改为按 `(packet_no, file_id)` 的传输索引 + 类型化 delta 枚举（Progress/Saved/Failed/Canceled）。

### P1-7 历史持久化：每事件全量重写 + async 上下文同步 IO

[app_state.rs:391-399](../src/app_state.rs#L391-L399)：上下线、每条消息都 `serde_json::to_string(全部历史)` + `fs::write`（std 阻塞 IO，直接在 async 任务里）。无节流、无 append、无 spawn_blocking、无历史上限。在线人数抖动频繁的局域网里这是持续的磁盘风暴。

### P1-8 render 有副作用，纯净性被破坏

[chat_shell.rs:449-450](../src/chat_shell.rs#L449-L450)：`render()` 内调用 `is_message_scroll_near_bottom` 写 `stick_to_bottom`、`clear_selected_unread_if_needed` 向 actor dispatch 命令。渲染函数应纯；副作用应挂在事件回调里。当前写法依赖渲染频率，行为不可复现。

### P2-1 协议实现健壮性缺陷集

| 缺陷 | 位置 | 说明 |
|---|---|---|
| TCP 请求头单次 read(4096) | [mod.rs:1561-1567](../src/ipmsg_core/mod.rs#L1561-L1567) | TCP 是字节流，请求分片/超 4096 即解析失败 |
| 去重表整体清空 | [mod.rs:379-381](../src/ipmsg_core/mod.rs#L379-L381) | `seen_msgs` 超 1 万条 clear() 全清，清空瞬间重复包全放行；应换 LRU |
| `now_millis()` 名不副实 | [mod.rs:553-570](../src/ipmsg_core/mod.rs#L553-L570) | 实为单调 packet_no 序列器，与名字无关，误导维护者 |
| 编码单一全局开关 | [mod.rs:36](../src/ipmsg_core/mod.rs#L36) | 收发共用 `TEXT_ENCODING`，无法"收 GB18030 发 UTF-8"；`IPMSG_UTF8OPT/CAPUTF8OPT` 常量定义了却从不检测/声明 |
| 目录递归无深度/符号链接防护 | [mod.rs:1467-1528](../src/ipmsg_core/mod.rs#L1467-L1528) | `metadata()` 跟随符号链接，环路目录可致无限递归 |
| `FILE_TABLE` 只增不清 | [mod.rs:275](../src/ipmsg_core/mod.rs#L275) | 已发文件条目永久驻留内存 |
| 前缀匹配用 `starts_with` | [mod.rs:636-661](../src/ipmsg_core/mod.rs#L636-L661) | 应使用 CIDR；无尾点时误匹配网段 |
| 接收端忽略 offset | [mod.rs:1576](../src/ipmsg_core/mod.rs#L1576) | 断点续传协议字段未实现 |

### P2-2 主 socket 回退逻辑疑似死代码

`send_message/send_file/send_files/send_folder/send_exit*` 六处复制粘贴"取 MAIN_SOCKET，否则重新 bind 2425"的样板（如 [mod.rs:855-869](../src/ipmsg_core/mod.rs#L855-L869)）。主 socket 已占用 2425 时，回退 bind 在多数平台必然 `AddrInUse` 失败——回退路径基本不可达，6 段重复代码应收敛为 `Service` 方法。

### P2-3 ChatShell 上帝对象 + 全量重渲染

ChatShell 持有搜索、输入、会话表、消息映射、滚动、语言、seq、滚动条共 10+ 组状态；任一变化 `cx.notify()` 触发四个面板整体重渲染。[chat_area.rs:29-270](../src/chat_area.rs#L29-L270) 单函数构造全部消息气泡，无虚拟化；长消息 `text_ellipsis + whitespace_nowrap` 直接截断不可展开。历史增长后渲染成本线性恶化。

### P2-4 会话/未读清除逻辑三处重复

未读清零分布在 `select_conversation`、轮询任务、`render()` 三处，条件（stick_to_bottom）相互纠缠，极易漂移出不一致。

### P3-1 零测试

协议编解码（`parse_packet`、`parse_u32_auto_radix`、目录头解析）、`addr_allowed`、网卡探测、会话分组逻辑全部可单测，但 `tests/` 不存在，`#![allow(dead_code)]`（[mod.rs:1](../src/ipmsg_core/mod.rs#L1)）进一步掩盖死代码（`send_file`、`send_exit`、`whoami`、`send_exit_to` 均未使用）。

### P3-2 错误吞噬成风

`let _ =` 全仓 30+ 处（含 `dispatch_cmd`、`persist_history`、broadcast send）。持久化失败用户无感知，历史可能静默丢失。

### P3-3 双重初始化与配置热路径

`main` 与 `init_state` 各 `load_config` 一次并各自 set_locale；8 处 `load_config()` 调用中含 250ms 轮询热路径（含隐式写盘副作用）。

---

## 4. 目标架构与演进路线

### 4.1 目标形态

```
crates/
  ipmsg-proto/    纯协议：Packet 编解码、常量、字节级 fuzz 目标（零 IO、零 i18n、零全局态）
  ipmsg-core/     Service 结构体持有 socket/file_table/user_info（实例化，非静态）
                  对外只暴露: Command / Event 两个通道 + async API
  app/            domain: UserId(稳定ID) / Message / Transfer，时间戳为 SystemTime
                  state:  真正 actor，EventEmitter 推送 delta，不做 i18n/不做持久化决策
                  persist: append-only JSONL + 节流落盘 + 容量上限（独立任务）
                  ui:     GPUI Entity 订阅事件，只渲染增量
```

核心原则：**事件推送替代轮询、实例替代全局、类型化身份替代字符串地址、append 替代全量重写**。

### 4.2 按优先级的整改清单

1. **立即**（不改架构）：
   - 修复 P0-1：`recv` 循环显式处理 `Lagged`（记录并继续）；
   - 修复 P0-2：filename 过滤（拒绝分隔符/`..`/盘符/UNC）；
   - 修复 P0-3：TCP 文件服务校验来源地址 == 目标接收者；file_id 改随机；
   - 移除 `Drop → process::exit`，退出流程显式 `send_exit()` 后关闭；
   - 发送失败入 UI 错误状态（消息带 pending/failed 标记）。
2. **短期**：
   - 消除 250ms 轮询：state actor 通过 GPUI `EventEmitter` 推 delta；UI 只保留滚动位置等本地态；
   - `ChatMessage.time` 改 `SystemTime`，渲染时才本地化；状态层去 `t!`；协议层去 `[文件夹]` 文案（改为 UI 依据 is_dir 生成）；
   - 历史改 JSONL append + 定时 flush + 上限；persist 移入 `spawn_blocking`；
   - `UpdateProgress` 拆类型化枚举 + 按 `(packet_no, file_id)` 建索引。
3. **中期**：
   - 全局静态收拢为 `Service`/`AppState` 实例，经 `cx`（GPUI Global 或 Entity）注入；
   - 消息三表示合并为单一领域模型，UI 层只做投影；
   - 拆 crate（proto/core/app）并补协议单测 + `cargo fuzz`；
   - 会话身份引入 `UN:` 稳定 ID；
   - RECVMSG 映射为 `Event::Delivered`，建立发送状态机（pending → delivered/failed + 超时重发）。

### 4.3 风险提示

当前单 crate、全局静态与轮询互相咬合，任何"只改一半"的增量（例如只把轮询改成推送、但保留全局快照）收益有限。建议按 §4.2 顺序推进，第 1 项与架构解耦可立即执行，第 2 项起需一次性划清 state→UI 的边界。

---

## 附录 A：静态事实

- 源文件 11 个，约 165 KB / 4600 行；最大文件 `ipmsg_core/mod.rs`（约 1700 行，协议/网络/文件服务/探测混杂）。
- `#[allow(dead_code)]` 2 处；未使用公开 API：`send_file`、`send_exit`、`send_exit_to`、`whoami`。
- 全局可变静态 13 个；`let _ =` 吞错 30+ 处；测试 0 个；`docs/` 本文档为首。
- 依赖：GPUI/gpui-component（git 快照，未锁 tag），`tokio full`（应裁剪 features）。

## 附录 B：整改记录

### 2026-09-04 P0 阶段修复（已完成，`cargo check`/`clippy` 通过）

| 问题 | 修复内容 | 位置 |
|---|---|---|
| P0-1 事件泵单点失效 | `recv` 循环显式处理 `Lagged`（记录丢失数并继续）与 `Closed` | [app_state.rs](../src/app_state.rs) `init_state` |
| P0-2 路径穿越 | 新增 `sanitize_wire_filename`（拒绝分隔符/`..`/`:`/控制字符/Windows 保留设备名）；接收侧 REGULAR 与非根 DIR 落盘前强制校验，非法即中止传输；发送侧跳过非法名条目 | [ipmsg_core/mod.rs](../src/ipmsg_core/mod.rs) |
| P0-3 文件服务无鉴权 | `FileEntry` 记录目标方 IP，TCP 请求方 IP 不匹配即拒绝（按 IP 比较：TCP 客户端来自临时端口）；`file_id` 由顺序递增改为 `rand::random`；取消令牌键统一为 `(IpAddr, packet_no, file_id)`（顺带修复取消上传令牌键不匹配导致永不生效的隐患）。**修正补充**：file_id 必须钳制在 `< 0x8000_0000`——对端（原版飞鸽/同类客户端）按**有符号 32 位**解析报文 id，≥ 2^31 会 ERANGE 溢出导致对方收不到文件条目；这与本仓库历史提交 `ea4320f "fileid兼容32位"` 对 packet_no 的钳制是同一条协议约束，已加单测防回归 | [ipmsg_core/mod.rs](../src/ipmsg_core/mod.rs) |
| P0-4 Drop→exit | 新增 `logic::shutdown()`：先经 runtime `block_on` 广播 BR_EXIT，再 `process::exit(0)`（GPUI 关闭末窗口后进程仍驻留，强退仍必需，但置于下线广播之后） | [logic.rs](../src/logic.rs) / [chat_shell.rs](../src/chat_shell.rs) |
| P0-5 发送失败静默 | `ChatMessage` 增加 `#[serde(default)] failed`；`send_text/send_files/send_folder` 失败同样入状态并携带标记；UI 对失败消息渲染 `danger` 边框与"发送失败"标签（复用 `transfer.send_failed`） | [app_state.rs](../src/app_state.rs) / [logic.rs](../src/logic.rs) / [chat_area.rs](../src/chat_area.rs) |

新增依赖：`rand = "0.9"`（file_id 随机化）。后续阶段（事件推送替代轮询、历史 JSONL 化、全局态收拢、RECVMSG 送达状态机）按 §4.2 推进。

### 2026-09-04 文件互传修复 + 送达状态机（已完成）

**根因 1（对端收不到文件条目 → 无 GETFILEDATA）**：SENDMSG 附件 `fileID:filename:size:mtime:attr` 中，协议规范（protocol.txt §5）只规定 size/mtime/attr 为 hex，**fileID 应为十进制**；此前 fileID 用 `{:x}` 输出，对端（飞鸽 6.1.200622 规范实现）按十进制解析失败 → 文件条目损坏 → 不发起下载。已改三处（send_file/send_files/send_folder）fileID 为 decimal。此前 P0-3 把 file_id 随机化 + 钳制到 31 位，与本次修复共同生效。

**根因 2（GETFILEDATA 解析脆弱）**：`handle_tcp_file` 原先仅按 hex 解析 `packet_no:file_id:offset`；部分国产客户端按 decimal 回显。新增 `lookup_file_entry`：对 pkt/file 各生成 hex、decimal、auto 三组候选值，逐一查 FILE_TABLE，命中即用（含 GETDIRFILES）。已加单测覆盖两种进制回显。

**送达状态机（原 P1 路线项提前落地）**：
- `Event::Delivered { from, packet_no }`：收到 IPMSG_RECVMSG 时以 extra 中回显的包号发出（替代原先的 Event::Unknown 丢弃）。
- `ChatMessage` 新增 `#[serde(default)] packet_no` 与 `delivered`；状态层匹配 `is_me && to==from && (packet_no 或 file.packet_no)` 置 delivered。
- `send_message` 返回实际 packet_no；logic 三处发送路径记录包号。
- UI 对已送达消息渲染绿色「已送达」（新增 i18n 键 `transfer.delivered`），failed 优先显示。

| 修复 | 位置 |
|---|---|
| fileID decimal 化 ×3 | [ipmsg_core/mod.rs](../src/ipmsg_core/mod.rs) send_file/send_files/send_folder |
| lookup_file_entry 宽容查表 | [ipmsg_core/mod.rs](../src/ipmsg_core/mod.rs) `handle_tcp_file` |
| Event::Delivered + RECVMSG | [ipmsg_core/mod.rs](../src/ipmsg_core/mod.rs) RECVMSG 分支 |
| ChatMessage.packet_no/delivered + 状态处理 | [app_state.rs](../src/app_state.rs) |
| 发送路径记录包号 | [logic.rs](../src/logic.rs) send_text/send_files/send_folder |
| 已送达 UI + i18n | [chat_area.rs](../src/chat_area.rs) / [locales](../locales) |

**遗留提醒**：若本次修复后对端点「接收」仍无动静，请抓包确认 TCP 2425 是否入站被 Windows 防火墙拦截（重建二进制后需重新放行），并核对日志是否有 `handle_tcp_file` 行。

### 2026-09-05 普通消息送达 + 失败重试（已完成）

**送达竞态修复**：RECVMSG（Delivered 事件）可能在 `PushOutgoing` 之前被事件泵处理——logic 任务在 send 完成与 dispatch 之间可能被抢占，Delivered 匹配不到消息后永远丢失。`CoreState.acked_packets`（有界 HashSet，上限 4096）兜底：Delivered 先记包号，PushOutgoing / RetryFinished 入状态时再查集合补置 `delivered`。

**失败重试**：
- `ChatMessage` 新增 `#[serde(default)] id`（历史加载时补发）；`StateCmd::RetryFinished { id, ok, packet_no, file_id }` 重发成功后原地更新原消息（failed→false、新包号/文件号、送达状态）。
- 文件/文件夹发送失败不再丢路径：失败气泡携带 `FileInfo.local_path`（send_files 逐路径一条失败气泡，send_folder 一条），可重发。
- `logic::retry_message(id, to, text, Option<(path, is_dir)>)`：文本走 SENDMSG，文件走 send_files/send_folder，成功后更新原气泡。
- UI：失败气泡红框 + 「发送失败」+ 「重试」按钮（新 i18n 键 `chat.retry`），文件失败显示文件名。

| 修复 | 位置 |
|---|---|
| id 字段 + acked_packets 竞态 + RetryFinished | [app_state.rs](../src/app_state.rs) |
| retry_message + 失败分支带路径 | [logic.rs](../src/logic.rs) |
| 视图模型 id/to + retry_message 方法 | [chat_shell.rs](../src/chat_shell.rs) |
| 重试按钮 + 文件失败显示 | [chat_area.rs](../src/chat_area.rs) |
| i18n chat.retry | [locales](../locales) |

**当前进度**：P0 全部 + P1 三项（送达状态机、失败可见化/重试、事件推送替代轮询）已完成；剩余 P1：历史 JSONL 化、全局态收拢、i18n 出状态/协议层。

### 2026-09-05 事件推送替代 250ms 轮询（已完成，`cargo check`/`clippy`/`test` 通过）

落实 §4.2 短期第 1 项：状态 actor 主动推 delta，UI 只保留选择/滚动等本地态。

- **状态层**：新增 `StateDelta`（`Sync`/`UsersChanged`/`MessageAdded`/`MessageUpdated`/`UnreadChanged`/`SettingsChanged`）与专用无界通道；`run_state_manager` 在每个变更点 `emit_delta`。状态层运行在 tokio 线程上无法持有 GPUI 上下文，故通道跨线程，由 UI 侧桥接任务在主线程转发——这是 `EventEmitter` 推送链路中不可省略的一环（actor 不是 GPUI Entity）。通道对在单一 `Lazy` 中原子创建，UI 先于 actor 接入也不存在竞态；初始 `Sync` 借助通道缓冲保序，UI 后接入也能拿到完整历史。
- **UI 层**：新增 `UiState` Entity（chat_shell.rs）消费 delta 并维护会话/消息视图模型，经 `EventEmitter<UiEvent>` 通知；ChatShell 经 `subscribe_in` 订阅，仅保留选择、滚动、输入草稿等本地态。会话列表重建成本 O(#users + #convs)，与历史消息总量无关；进度等单条更新按消息 `id` 原地替换，不再全量克隆。**250ms 轮询任务、`STATE_SEQ`/`state_seq`、`list_messages`/`list_online_users`/`list_unread_counts` 全部删除**（`STATE_SNAPSHOT` 仍供 logic 同步读取自地址）。
- **消息身份**：收到的消息（`Event::Message`/`FileOffer`）也分配稳定 `id`，`MessageUpdated` 以 id 定位，UI 增量替换不再依赖倒序全表扫描。
- **语言切换**：`logic::save_settings` → `StateCmd::SettingsSaved` → delta → ChatShell 刷新占位文案；删除了轮询中每 250ms 读 config.toml 的探测（连带消除其隐式写盘副作用）。
- **会话选择**：改为按稳定 id（SocketAddr 字符串）而非列表下标，列表重排不再丢失选中。

| 变更 | 位置 |
|---|---|
| StateDelta + 推送通道 + 各变更点 emit | [app_state.rs](../src/app_state.rs) |
| UiState Entity（EventEmitter）+ 桥接任务 + 按 id 选择 | [chat_shell.rs](../src/chat_shell.rs) |
| 会话列表/聊天区改读 UiState | [conversation_list.rs](../src/conversation_list.rs) / [chat_area.rs](../src/chat_area.rs) |
| SettingsSaved 通知链 | [logic.rs](../src/logic.rs) |

### 2026-09-05 中期项 1+2：全局静态收拢 + 消息单一领域模型（已完成，`cargo check`/`clippy`/`test` 通过）

落实 §4.2 中期第 1、2 项。

**全局静态收拢为实例（中期项 1）**
- `ipmsg_core`：新增 `Service` 结构体，收拢原 `MAIN_SOCKET`/`NET_CONFIG`/`USER_INFO`/`TEXT_ENCODING`/`FILE_TABLE`/`FILE_ID_SEQ`/`PACKET_NO_SEQ`/`SEND_CANCEL_FLAGS`/`EXT_ID_PART` 等全局态；原先读写这些全局的自由函数（`send_message`/`send_files`/`recv_file`/`handle_udp_packet`/`addr_allowed`/`broadcast_target` 等约 20 个）改为 `Service` 方法，经 `&self` 访问状态。`OnceLock<Arc<Service>>` 单例由 `start_ipmsg()` 写入，`service()`/`try_service()` 供 tokio 任务取用（后者在未启动/启动失败时返回 None，供退出流程安全跳过）。UDP/TCP 常驻任务各自持有 `Arc<Service>` 克隆。
- `app_state`：`STATE_SNAPSHOT`/`STATE_CMD_TX`/`STATE_DELTA`/`NEXT_MESSAGE_ID` 收进 `AppState` 实例（命令/增量通道、快照、消息 id 序列、`active_downloads` 下载任务表），`init_state()` 改为 `AppState::init()` 方法。UI 侧经 GPUI Global 注入：`AppStateGlobal(pub Arc<AppState>)` newtype（`Arc` 是外部类型，按孤儿规则不能直接实现 `Global`，套 newtype 是 GPUI Global 文档推荐做法），`ChatShell::new` 中 `cx.set_global`，设置窗口等主线程代码经 `cx.global::<AppStateGlobal>()` 读取——UI 层不再触碰任何全局静态。
- `logic`：发送/下载/设置函数全部改为接收 `&Arc<AppState>`；`ACTIVE_DOWNLOADS` 静态并入 `AppState.active_downloads`；`ensure_started` 在 runtime 线程之外同步创建 `AppState` 并 `set_instance`，协议服务 `start_ipmsg` 由事件泵任务（`AppState::pump_events`）在 runtime 内启动——消除"UI 先 dispatch 而全局未初始化即 panic"的隐式初始化顺序问题。
- `sidebar`：`SETTINGS_WINDOW_HANDLE` 静态改为 `SettingsWindowHandle` GPUI Global。

**消息三表示合并为单一领域模型（中期项 2）**
- 删除 `chat_shell::ChatMessage`/`FileTransfer` 第二、三套表示；`UiState` 直接存储 `app_state::ChatMessage`/`FileInfo` 领域模型。
- UI 渲染只做投影：新增 `display_text` helper 依据附件类型生成气泡文案（文件夹/文件前缀），替代此前状态层、视图层、逻辑层多处重复拼接；`chat_area`/`conversation_list` 同步改读领域模型。

**剩余全局态**：仅 4 处且全部"写一次读多次"——初始化守卫 `RUNTIME_HANDLE`/`STARTED`（OnceCell）与单例注册器 `APP_STATE`/`SERVICE`（OnceLock）；原 13 个运行时可变全局已全部消除。

| 变更 | 位置 |
|---|---|
| Service 结构体 + ~20 函数方法化 + 单例 | [ipmsg_core/mod.rs](../src/ipmsg_core/mod.rs) |
| AppState 实例 + AppStateGlobal + 方法化 | [app_state.rs](../src/app_state.rs) |
| `&Arc<AppState>` 参数 + ensure_started 同步建实例 + 下载表迁移 | [logic.rs](../src/logic.rs) |
| 删除两套消息表示 + display_text 投影 | [chat_shell.rs](../src/chat_shell.rs) |
| 渲染切片改读领域模型 | [chat_area.rs](../src/chat_area.rs) / [conversation_list.rs](../src/conversation_list.rs) |
| SettingsWindowHandle GPUI Global | [sidebar.rs](../src/sidebar.rs) |

**当前进度**：P0 全部 + P1 五项（送达状态机、失败可见化/重试、事件推送替代轮询、全局态收拢、消息单一模型）已完成；剩余 P1：历史 JSONL 化、`UpdateProgress` 类型化、i18n 出状态/协议层、`UN:` 稳定身份、render 副作用清理。
