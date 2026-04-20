# gpui-ipmsg

基于 Rust + [GPUI](https://github.com/zed-industries/zed) 实现的局域网飞鸽（IP Messenger）客户端，支持文字聊天与文件/文件夹传输。

## 功能特性

- 局域网在线用户发现（上线/下线广播）
- 点对点文本消息收发
- 文件发送与接收
- 文件夹发送与接收
- 传输进度展示与下载取消
- 用户名、分组与文本编码（`UTF-8` / `GB18030`）配置

## 技术栈

- Rust（Edition 2024）
- [gpui](https://github.com/zed-industries/zed)
- [gpui-component](https://github.com/longbridge/gpui-component)
- tokio（异步网络）
- serde + toml（配置持久化）

## 快速开始

### 1. 准备环境

- 安装 Rust（建议使用最新 stable）  
  [https://www.rust-lang.org/tools/install](https://www.rust-lang.org/tools/install)

### 2. 获取代码

```bash
git clone <your-repo-url>
cd gpui-ipmsg
```

### 3. 运行

```bash
cargo run
```

### 4. 打包（可选）

```bash
cargo build --release
```

## 配置文件

程序会自动读取/生成配置文件 `config.toml`。

- Windows: `%APPDATA%/gpui-ipmsg/config.toml`
- macOS: `~/Library/Application Support/gpui-ipmsg/config.toml`
- Linux: `$XDG_CONFIG_HOME/gpui-ipmsg/config.toml` 或 `~/.config/gpui-ipmsg/config.toml`

示例：

```toml
[user]
username = "alice"
group = "开发组"

language = "GB18030"
```

> `language` 可选值：`UTF-8`、`GB18030`。

## 项目结构

```text
src/
  main.rs               # 应用入口与窗口初始化
  app_state.rs          # 全局状态与消息分发
  chat_shell.rs         # 主聊天容器
  conversation_list.rs  # 会话列表
  chat_area.rs          # 消息展示区域
  input_area.rs         # 输入与发送区域
  sidebar.rs            # 侧边栏
  logic.rs              # UI 与网络逻辑桥接
  config.rs             # 配置读写与路径处理
  ipmsg_core/
    mod.rs              # IPMSG 协议与网络核心实现
    protocol.rs         # 协议常量定义
```

## 协议与网络说明

- 默认使用 IPMSG 端口：`2425`
- 基于 UDP 进行在线广播和消息控制
- 基于 TCP 进行文件/文件夹传输

## 开发建议

- 使用 `cargo check` 做快速编译检查
- 使用 `cargo clippy` 做静态检查
- 使用 `cargo fmt` 统一代码格式

## 说明

该项目当前处于持续迭代阶段，协议兼容性与功能完整度会逐步完善。
