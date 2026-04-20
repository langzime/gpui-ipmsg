# gpui-ipmsg

[中文说明](./README.md)

A LAN IP Messenger client built with Rust + [GPUI](https://github.com/zed-industries/zed), supporting text chat and file/folder transfer.

## Features

- LAN peer discovery (online/offline broadcast)
- Point-to-point text messaging
- File sending and receiving
- Folder sending and receiving
- Transfer progress display and cancel support
- User profile and text encoding settings (`UTF-8` / `GB18030`)

## Tech Stack

- Rust (Edition 2024)
- [gpui](https://github.com/zed-industries/zed)
- [gpui-component](https://github.com/longbridge/gpui-component)
- tokio (async networking)
- serde + toml (config persistence)

## Quick Start

### 1. Prerequisites

- Install Rust (latest stable is recommended)  
  [https://www.rust-lang.org/tools/install](https://www.rust-lang.org/tools/install)

### 2. Clone

```bash
git clone <your-repo-url>
cd gpui-ipmsg
```

### 3. Run

```bash
cargo run
```

### 4. Build (Optional)

```bash
cargo build --release
```

## Configuration

The app automatically reads/creates `config.toml`.

- Windows: `%APPDATA%/gpui-ipmsg/config.toml`
- macOS: `~/Library/Application Support/gpui-ipmsg/config.toml`
- Linux: `$XDG_CONFIG_HOME/gpui-ipmsg/config.toml` or `~/.config/gpui-ipmsg/config.toml`

Example:

```toml
[user]
username = "alice"
group = "Dev Team"

language = "GB18030"
```

> Available values for `language`: `UTF-8`, `GB18030`.

## Project Structure

```text
src/
  main.rs               # App entry and window initialization
  app_state.rs          # Global state and command dispatch
  chat_shell.rs         # Main chat container
  conversation_list.rs  # Conversation list panel
  chat_area.rs          # Message display area
  input_area.rs         # Input and sending area
  sidebar.rs            # Sidebar
  logic.rs              # Bridge between UI and network logic
  config.rs             # Config load/save and path handling
  ipmsg_core/
    mod.rs              # IPMSG protocol and network core
    protocol.rs         # Protocol constants
```

## Protocol & Network

- Default IPMSG port: `2425`
- UDP for online broadcast and message control
- TCP for file/folder transfer

## Development Tips

- Run `cargo check` for quick compile checks
- Run `cargo clippy` for linting
- Run `cargo fmt` for formatting

## Notes

This project is under active development, and protocol compatibility and feature coverage will continue to improve.
