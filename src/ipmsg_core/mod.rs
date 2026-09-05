#![allow(dead_code)]
pub mod protocol;
use protocol::*;
use anyhow::{anyhow, Result};
use encoding_rs::{Encoding, GB18030, UTF_8};
use get_if_addrs::get_if_addrs;
use log::{info, warn};
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct Packet {
    pub version: u32,
    pub packet_no: u32,
    pub username: String,
    pub hostname: String,
    pub command: u32,
    pub extra: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Gb18030,
}

static TEXT_ENCODING: Lazy<RwLock<TextEncoding>> = Lazy::new(|| RwLock::new(TextEncoding::Gb18030));

fn current_text_encoding() -> TextEncoding {
    *TEXT_ENCODING.read().unwrap()
}

fn current_codec() -> &'static Encoding {
    match current_text_encoding() {
        TextEncoding::Utf8 => UTF_8,
        TextEncoding::Gb18030 => GB18030,
    }
}

fn encode_text(s: &str) -> Vec<u8> {
    let (buf, _, _) = current_codec().encode(s);
    buf.into_owned()
}

fn decode_text(buf: &[u8]) -> String {
    let (s, _, _) = current_codec().decode(buf);
    s.into_owned()
}

pub fn set_text_encoding(encoding: TextEncoding) {
    let mut current = TEXT_ENCODING.write().unwrap();
    *current = encoding;
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        let s = format!(
            "{}:{}:{}:{}:{}:{}",
            self.version,
            self.packet_no,
            self.username,
            self.hostname,
            self.command,
            self.extra
        );
        info!("encode: {:?} ", s);
        encode_text(&s)
    }
}

pub fn parse_packet(buf: &[u8]) -> Result<Packet> {
    let s = decode_text(buf);
    info!("parse: {:?} ", s);
    let parts = s.splitn(6, ':').collect::<Vec<_>>();
    if parts.len() < 5 {
        return Err(anyhow!("bad packet"));
    }
    let version = parts[0].trim().parse::<u32>().unwrap_or(0);
    let packet_no = parts[1].trim().parse::<u32>()?;
    let username = parts[2].to_string();
    let hostname = parts[3].to_string();
    let command = parts[4].trim().parse::<u32>()?;
    let extra = if parts.len() >= 6 { parts[5].to_string() } else { String::new() };
    Ok(Packet {
        version,
        packet_no,
        username,
        hostname,
        command,
        extra,
    })
}

fn split_extra(extra: &str) -> (String, Vec<String>) {
    let mut iter = extra.split('\0');
    let main = iter.next().unwrap_or("").to_string();
    let rest = iter.filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    (main, rest)
}

/// Validate a single path component received from the network before it ever
/// touches the local filesystem. Rejects separators, parent references,
/// drive/ADS colons, control characters and Windows reserved device names
/// (P0-2 in docs/architecture.md).
fn sanitize_wire_filename(name: &str) -> Option<&str> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name
        .chars()
        .any(|c| c.is_control() || c == '/' || c == '\\' || c == ':')
    {
        return None;
    }
    let stem = name.split('.').next().unwrap_or("");
    match stem.to_ascii_uppercase().as_str() {
        "CON" | "PRN" | "AUX" | "NUL"
        | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6" | "COM7" | "COM8" | "COM9"
        | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9" => {
            return None;
        }
        _ => {}
    }
    Some(name)
}

fn parse_entry_extra(username: &str, extra: &str) -> (String, String) {
    let mut iter = extra.split('\0');
    let nick = iter.next().unwrap_or("");
    let group = iter.next().unwrap_or("");

    let clean_nick: String = nick.chars().filter(|c| !c.is_control()).collect();
    let clean_nick = clean_nick.trim();
    let final_nick = if !clean_nick.is_empty() {
        clean_nick.to_string()
    } else {
        username.to_string()
    };

    let clean_group: String = group.chars().filter(|c| !c.is_control()).collect();
    let final_group = clean_group.trim().to_string();

    (final_nick, final_group)
}
fn parse_u32_auto_radix(s: &str) -> u32 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).unwrap_or(0);
    }
    let has_hex_alpha = s
        .as_bytes()
        .iter()
        .any(|b| (*b >= b'a' && *b <= b'f') || (*b >= b'A' && *b <= b'F'));
    if has_hex_alpha {
        return u32::from_str_radix(s, 16).unwrap_or(0);
    }
    s.parse::<u32>()
        .or_else(|_| u32::from_str_radix(s, 16))
        .unwrap_or(0)
}

fn parse_u64_auto_radix(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).unwrap_or(0);
    }
    let has_hex_alpha = s
        .as_bytes()
        .iter()
        .any(|b| (*b >= b'a' && *b <= b'f') || (*b >= b'A' && *b <= b'F'));
    if has_hex_alpha {
        return u64::from_str_radix(s, 16).unwrap_or(0);
    }
    s.parse::<u64>()
        .or_else(|_| u64::from_str_radix(s, 16))
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub enum Event {
    Online { user: String, group: String, host: String, addr: SocketAddr },
    Offline { user: String, host: String, addr: SocketAddr },
    Message { from: SocketAddr, user: String, host: String, text: String },
    FileOffer {
        from: SocketAddr,
        user: String,
        host: String,
        packet_no: u32,
        file_id: u32,
        name: String,
        size: u64,
        is_dir: bool,
    },
    FileServed {
        to: SocketAddr,
        packet_no: u32,
        file_id: u32,
        is_dir: bool,
    },
    FileServingStarted {
        to: SocketAddr,
        packet_no: u32,
        file_id: u32,
        is_dir: bool,
    },
    /// Peer's client acknowledged receiving our SENDMSG (IPMSG_RECVMSG).
    /// `packet_no` is the id of the packet WE sent; matched against outgoing
    /// text/file messages in the state layer.
    Delivered {
        from: SocketAddr,
        packet_no: u32,
    },
    Unknown { from: SocketAddr, raw: String },
}

#[derive(Debug, Clone)]
pub struct SentFileMeta {
    pub packet_no: u32,
    pub file_id: u32,
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

pub struct Service {
    pub socket: Arc<UdpSocket>,
    pub events: broadcast::Sender<Event>,
    pub port: u16,
}

static NET_CONFIG: Lazy<Option<(String, Ipv4Addr)>> = Lazy::new(detect_net);
static MAIN_SOCKET: Lazy<Mutex<Option<Arc<UdpSocket>>>> = Lazy::new(|| Mutex::new(None));

pub struct UserInfo {
    pub username: String,
    pub hostname: String,
    pub group: String,
}

static USER_INFO: Lazy<RwLock<UserInfo>> = Lazy::new(|| {
    let username = whoami::username();
    let hostname = hostname::get().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|_| "host".into());
    RwLock::new(UserInfo {
        username,
        hostname,
        group: "".to_string(),
    })
});

pub fn set_user_info(name: &str, group: &str) {
    let mut info = USER_INFO.write().unwrap();
    info.username = name.to_string();
    info.group = group.to_string();
}

static EXT_ID_PART: Lazy<String> = Lazy::new(|| {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{:016x}", t as u64)
});

fn build_user_extra() -> String {
    let info = USER_INFO.read().unwrap();
    let mut extra = String::new();
    extra.push_str(&info.username);
    extra.push('\0');
    extra.push_str(&info.group);
    extra.push('\0');
    extra.push_str("UN:");
    extra.push_str(&format!("{}-<{}>", info.username, *EXT_ID_PART));
    extra.push('\0');
    extra.push_str("HN:");
    extra.push_str(&info.hostname);
    extra.push('\0');
    extra.push_str("NN:");
    extra.push_str(&info.username);
    extra.push('\0');
    extra.push_str("GN:");
    extra.push_str(&info.group);
    extra.push('\0');
    extra.push_str("VS:00010002:5:7:2");
    extra
}

#[derive(Clone, Debug)]
struct FileEntry {
    /// IP of the peer this file was offered to. Any other requester is denied
    /// (P0-3 in docs/architecture.md).
    to: IpAddr,
    path: PathBuf,
    size: u64,
    is_dir: bool,
}

static FILE_TABLE: Lazy<Mutex<HashMap<(u32, u32), FileEntry>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static PACKET_NO_SEQ: AtomicU32 = AtomicU32::new(1);

/// Identifies an in-flight outgoing transfer: destination host (IP only — the
/// TCP peer connects from an ephemeral port), plus the offered packet/file ids.
type CancelKey = (IpAddr, u32, u32);
static SEND_CANCEL_FLAGS: Lazy<Mutex<HashMap<CancelKey, Arc<AtomicBool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Random, unguessable file transfer id (sequential ids were trivially
/// enumerable by any LAN host).
///
/// MUST stay below 0x8000_0000: peers parse ids with *signed* 32-bit
/// arithmetic, so values >= 2^31 overflow (strtol ERANGE) and the whole file
/// entry becomes unreceivable on original IPMsg clients. Same constraint as
/// packet_no — see commit ea4320f "fileid兼容32位".
fn random_file_id() -> u32 {
    rand::random::<u32>() & 0x7fff_ffff
}

fn send_cancel_token(peer: IpAddr, packet_no: u32, file_id: u32) -> Arc<AtomicBool> {
    let mut flags = SEND_CANCEL_FLAGS.lock().unwrap();
    flags
        .entry((peer, packet_no, file_id))
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

fn clear_send_cancel_token(peer: IpAddr, packet_no: u32, file_id: u32) {
    SEND_CANCEL_FLAGS
        .lock()
        .unwrap()
        .remove(&(peer, packet_no, file_id));
}

pub fn cancel_send(to: SocketAddr, packet_no: u32, file_id: u32) -> bool {
    let removed = FILE_TABLE.lock().unwrap().remove(&(packet_no, file_id)).is_some();
    let token = send_cancel_token(to.ip(), packet_no, file_id);
    token.store(true, Ordering::SeqCst);
    removed
}

impl Service {
    pub async fn new() -> Result<Self> {
        let port = PORT;
        let raw = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port)).await?;
        raw.set_broadcast(true)?;
        let socket = Arc::new(raw);
        {
            let mut g = MAIN_SOCKET.lock().unwrap();
            *g = Some(socket.clone());
        }
        let (tx, _) = broadcast::channel(64);
        Ok(Self { socket, events: tx, port })
    }

    pub async fn spawn(&self) -> Result<()> {
        self.broadcast_entry().await?;
        let socket = self.socket.clone();
        let tx_udp = self.events.clone();
        let tx_tcp = self.events.clone();
        let port = self.port;
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let mut seen_msgs: HashSet<(IpAddr, u32)> = HashSet::new();
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        let data = &buf[..n];
                        match parse_packet(data) {
                            Ok(p) => {
                                let base = p.command & 0x000000ff;
                                let opts = p.command & !0x000000ff;
                                match base {
                                    IPMSG_BR_ENTRY => {
                                        info!(
                                            "recv BR_ENTRY id={} from {}@{} ({}) extra='{:?}'",
                                            p.packet_no, p.username, p.hostname, from, p.extra
                                        );
                                        let (user, group) = parse_entry_extra(&p.username, &p.extra);
                                        if addr_allowed(&from) {
                                            let _ = tx_udp.send(Event::Online { user, group, host: p.hostname.clone(), addr: from });
                                        }
                                        let _ = send_ansentry(&socket, from).await;
                                    }
                                    IPMSG_ANSENTRY => {
                                        info!(
                                            "recv ANSENTRY id={} from {}@{} ({}) extra='{:?}'",
                                            p.packet_no, p.username, p.hostname, from, p.extra
                                        );
                                        let (user, group) = parse_entry_extra(&p.username, &p.extra);
                                        if addr_allowed(&from) {
                                            let _ = tx_udp.send(Event::Online { user, group, host: p.hostname.clone(), addr: from });
                                        }
                                    }
                                    IPMSG_BR_EXIT => {
                                        info!(
                                            "recv BR_EXIT id={} from {}@{} ({})",
                                            p.packet_no, p.username, p.hostname, from
                                        );
                                        if addr_allowed(&from) {
                                            let _ = tx_udp.send(Event::Offline { user: p.username.clone(), host: p.hostname.clone(), addr: from });
                                        }
                                    }
                                    IPMSG_SENDMSG => {
                                        let (main_text, ext) = split_extra(&p.extra);
                                        let sealed = (opts & IPMSG_SECRETOPT) != 0;
                                        let encrypted = (opts & IPMSG_ENCRYPTOPT) != 0;
                                        let need_check = (opts & IPMSG_SENDCHECKOPT) != 0;
                                        let has_file = (opts & IPMSG_FILEATTACHOPT) != 0;
                                        let key_ip = match from {
                                            SocketAddr::V4(v4) => IpAddr::V4(*v4.ip()),
                                            SocketAddr::V6(v6) => IpAddr::V6(*v6.ip()),
                                        };
                                        let key = (key_ip, p.packet_no);
                                        let duplicated = seen_msgs.contains(&key);
                                        if !duplicated {
                                            if seen_msgs.len() > 10000 {
                                                seen_msgs.clear();
                                            }
                                            seen_msgs.insert(key);
                                            info!(
                                                "recv SENDMSG id={} from {}@{} ({}) sealed={} encrypted={} text='{}' ext={:?}",
                                                p.packet_no, p.username, p.hostname, from, sealed, encrypted, main_text, ext
                                            );
                                            if addr_allowed(&from) {
                                                let text = if encrypted {
                                                    "[加密消息，暂不支持解密]".to_string()
                                                } else {
                                                    main_text.clone()
                                                };
                                                let _ = tx_udp.send(Event::Message { from, user: p.username.clone(), host: p.hostname.clone(), text });
                                                if has_file {
                                                    let mut files = Vec::new();
                                                    for e in ext.iter() {
                                                        for item in e.split('\x07') {
                                                            let item = item.trim();
                                                            if item.is_empty() {
                                                                continue;
                                                            }
                                                            let parts: Vec<&str> = item.split(':').collect();
                                                            if parts.len() < 5 {
                                                                continue;
                                                            }
                                                            let id_str = parts[0].trim();
                                                            let fid = parse_u32_auto_radix(id_str);
                                                            let name = parts[1].replace("::", ":");
                                                            let size = parse_u64_auto_radix(parts[2].trim());
                                                            let attr_hex = parts[4].trim();
                                                            let attr_val = parse_u32_auto_radix(attr_hex);
                                                            let file_type = attr_val & 0x000000ff;
                                                            let is_dir = file_type == IPMSG_FILE_DIR;
                                                            files.push((fid, name, size, is_dir));
                                                        }
                                                    }
                                                    for (fid, name, size, is_dir) in files {
                                                        let _ = tx_udp.send(Event::FileOffer {
                                                            from,
                                                            user: p.username.clone(),
                                                            host: p.hostname.clone(),
                                                            packet_no: p.packet_no,
                                                            file_id: fid,
                                                            name,
                                                            size,
                                                            is_dir,
                                                        });
                                                    }
                                                }
                                            }
                                        } else {
                                            info!(
                                                "recv DUP SENDMSG id={} from {}@{} ({}) ignored for view",
                                                p.packet_no, p.username, p.hostname, from
                                            );
                                        }
                                        if need_check {
                                            let _ = send_recvmsg(&socket, p.packet_no, from).await;
                                        }
                                        if sealed {
                                            let _ = send_readmsg(&socket, p.packet_no, from).await;
                                        }
                                    }
                                    IPMSG_RECVMSG => {
                                        let (main, _) = split_extra(&p.extra);
                                        info!(
                                            "recv RECVMSG id={} from {}@{} ({}) packet_no={}",
                                            p.packet_no, p.username, p.hostname, from, main
                                        );
                                        if addr_allowed(&from) {
                                            // main is the packet_no of the SENDMSG we sent to this
                                            // peer; deliver it as a delivery confirmation.
                                            let acked = parse_u32_auto_radix(&main);
                                            let _ = tx_udp.send(Event::Delivered {
                                                from,
                                                packet_no: acked,
                                            });
                                        }
                                    }
                                    IPMSG_NOOPERATION => {
                                        info!(
                                            "recv NOOP command={} from {} raw='{:?}'",
                                            p.command,
                                            from,
                                            String::from_utf8_lossy(data)
                                        );
                                    }
                                    _ => {
                                        info!(
                                            "recv UNKNOWN command={} from {} raw='{:?}'",
                                            p.command,
                                            from,
                                            String::from_utf8_lossy(data)
                                        );
                                        if addr_allowed(&from) {
                                            let _ = tx_udp.send(Event::Unknown { from, raw: String::from_utf8_lossy(data).to_string() });
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(
                                            "failed to parse packet from {}: {} raw='{:?}'",
                                            from,
                                            e,
                                            String::from_utf8_lossy(data)
                                        );
                                if addr_allowed(&from) {
                                    let _ = tx_udp.send(Event::Unknown { from, raw: String::from_utf8_lossy(data).to_string() });
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(code) = e.raw_os_error() {
                            if code == 10054 {
                                info!("ignore udp reset error: {}", e);
                                continue;
                            }
                        }
                        warn!("{}", e);
                    }
                }
            }
        });
        tokio::spawn(async move {
            let listener = match TcpListener::bind(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                port,
            ))
            .await
            {
                Ok(l) => l,
                Err(e) => {
                    warn!("failed to bind TCP listener on {}: {}", port, e);
                    return;
                }
            };
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        let tx = tx_tcp.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_tcp_file(stream, addr, tx).await {
                                warn!("tcp file handler error from {}: {}", addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        warn!("tcp accept error: {}", e);
                    }
                }
            }
        });
        Ok(())
    }

    async fn broadcast_entry(&self) -> Result<()> {
        let (username, hostname) = {
            let info = USER_INFO.read().unwrap();
            (info.username.clone(), info.hostname.clone())
        };
        let extra = build_user_extra();
        let packet = Packet {
            version: VER,
            packet_no: now_millis(),
            username,
            hostname,
            command: IPMSG_BR_ENTRY,
            extra,
        };
        let addr = broadcast_target();
        self.socket.send_to(&packet.encode(), addr).await?;
        info!("BR_ENTRY sent id={} to {}", packet.packet_no, addr);
        Ok(())
    }
}

fn now_millis() -> u32 {
    const MAX_PACKET_NO: u32 = 0x7fff_ffff;
    loop {
        let current = PACKET_NO_SEQ.load(Ordering::Relaxed);
        let current = if current == 0 || current > MAX_PACKET_NO {
            1
        } else {
            current
        };
        let next = if current >= MAX_PACKET_NO { 1 } else { current + 1 };
        if PACKET_NO_SEQ
            .compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return current;
        }
    }
}

async fn send_ansentry(socket: &UdpSocket, to: SocketAddr) -> Result<()> {
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let extra = build_user_extra();
    let packet = Packet {
        version: VER,
        packet_no: now_millis(),
        username,
        hostname,
        command: IPMSG_ANSENTRY,
        extra,
    };
    socket.send_to(&packet.encode(), to).await?;
    Ok(())
}

async fn send_recvmsg(socket: &UdpSocket, packet_no: u32, to: SocketAddr) -> Result<()> {
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let packet = Packet {
        version: VER,
        packet_no: now_millis(),
        username,
        hostname,
        command: IPMSG_RECVMSG,
        extra: packet_no.to_string(),
    };
    socket.send_to(&packet.encode(), to).await?;
    info!(
        "RECVMSG sent id={} to {} packet_no={}",
        packet.packet_no, to, packet_no
    );
    Ok(())
}

async fn send_readmsg(socket: &UdpSocket, packet_no: u32, to: SocketAddr) -> Result<()> {
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let packet = Packet {
        version: VER,
        packet_no: now_millis(),
        username,
        hostname,
        command: IPMSG_READMSG,
        extra: packet_no.to_string(),
    };
    socket.send_to(&packet.encode(), to).await?;
    info!(
        "READMSG sent id={} to {} packet_no={}",
        packet.packet_no, to, packet_no
    );
    Ok(())
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

fn addr_allowed(addr: &SocketAddr) -> bool {
    if let Ok(prefix) = std::env::var("IPMSG_NET_PREFIX") {
        if let SocketAddr::V4(v4) = addr {
            let ip_str = v4.ip().to_string();
            if !ip_str.starts_with(&prefix) {
                info!(
                    "ignore packet from {} not in prefix {}",
                    ip_str, prefix
                );
                return false;
            }
        }
    } else if let SocketAddr::V4(v4) = addr {
        if let Some((ref prefix, _)) = *NET_CONFIG {
            let ip_str = v4.ip().to_string();
            if !ip_str.starts_with(prefix) {
                info!(
                    "ignore packet from {} not in auto prefix {}",
                    ip_str, prefix
                );
                return false;
            }
        }
    }
    true
}

fn broadcast_target() -> SocketAddr {
    if let Ok(s) = std::env::var("IPMSG_BROADCAST_ADDR") {
        if let Ok(ip) = s.parse::<Ipv4Addr>() {
            return SocketAddr::new(IpAddr::V4(ip), PORT);
        }
    }
    if let Some((_, bcast)) = *NET_CONFIG {
        return SocketAddr::new(IpAddr::V4(bcast), PORT);
    }
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)), PORT)
}

fn detect_net() -> Option<(String, Ipv4Addr)> {
    let ifs = get_if_addrs().ok()?;
    let mut pref_192 = Vec::new();
    let mut pref_other = Vec::new();
    for iface in ifs {
        if is_virtual_name(&iface.name) {
            continue;
        }
        let ip = iface.ip();
        if let IpAddr::V4(v4) = ip {
            if v4.is_loopback() {
                continue;
            }
            if !is_private(v4) {
                continue;
            }
            let o = v4.octets();
            let prefix = format!("{}.{}.{}.", o[0], o[1], o[2]);
            let bcast = Ipv4Addr::new(o[0], o[1], o[2], 255);
            if o[0] == 192 && o[1] == 168 {
                pref_192.push((v4, prefix, bcast));
            } else {
                pref_other.push((v4, prefix, bcast));
            }
        }
    }
    if let Some((ip, prefix, bcast)) = pref_192.into_iter().next() {
        info!("auto net detect ip={} prefix={} broadcast={}", ip, prefix, bcast);
        return Some((prefix, bcast));
    }
    if let Some((ip, prefix, bcast)) = pref_other.into_iter().next() {
        info!("auto net detect ip={} prefix={} broadcast={}", ip, prefix, bcast);
        return Some((prefix, bcast));
    }
    None
}

pub fn detect_self_addr(port: u16) -> Option<SocketAddr> {
    let ifs = get_if_addrs().ok()?;
    let mut pref_192 = Vec::new();
    let mut pref_other = Vec::new();
    for iface in ifs {
        if is_virtual_name(&iface.name) {
            continue;
        }
        let ip = iface.ip();
        if let IpAddr::V4(v4) = ip {
            if v4.is_loopback() {
                continue;
            }
            if !is_private(v4) {
                continue;
            }
            if v4.octets()[0] == 192 && v4.octets()[1] == 168 {
                pref_192.push(v4);
            } else {
                pref_other.push(v4);
            }
        }
    }
    if let Some(ip) = pref_192.into_iter().next() {
        return Some(SocketAddr::new(IpAddr::V4(ip), port));
    }
    if let Some(ip) = pref_other.into_iter().next() {
        return Some(SocketAddr::new(IpAddr::V4(ip), port));
    }
    None
}

fn is_private(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    match o[0] {
        10 => true,
        172 => (16..=31).contains(&o[1]),
        192 => o[1] == 168,
        _ => false,
    }
}

fn is_virtual_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("vmware")
        || n.contains("vbox")
        || n.contains("virtual")
        || n.contains("hyper-v")
        || n.contains("hyperv")
}

pub async fn send_broadcast_entry() -> Result<()> {
    let socket = {
        MAIN_SOCKET.lock().unwrap().as_ref().cloned()
    };
    if let Some(socket) = socket {
        let (username, hostname) = {
            let info = USER_INFO.read().unwrap();
            (info.username.clone(), info.hostname.clone())
        };
        let extra = build_user_extra();
        let packet = Packet {
            version: VER,
            packet_no: now_millis(),
            username,
            hostname,
            command: IPMSG_BR_ENTRY,
            extra,
        };
        let addr = broadcast_target();
        socket.send_to(&packet.encode(), addr).await?;
        info!("BR_ENTRY sent id={} to {}", packet.packet_no, addr);
    }
    Ok(())
}

pub async fn send_exit() -> Result<()> {
    let socket = {
        MAIN_SOCKET.lock().unwrap().as_ref().cloned()
    };
    let socket = if let Some(s) = socket {
        s
    } else {
        let raw = UdpSocket::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            PORT,
        ))
        .await?;
        raw.set_broadcast(true)?;
        Arc::new(raw)
    };
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let extra = build_user_extra();
    let packet = Packet {
        version: VER,
        packet_no: now_millis(),
        username,
        hostname,
        command: IPMSG_BR_EXIT,
        extra,
    };
    let addr = broadcast_target();
    socket.send_to(&packet.encode(), addr).await?;
    info!("BR_EXIT sent id={} to {}", packet.packet_no, addr);
    Ok(())
}

pub async fn send_exit_to(to: SocketAddr) -> Result<()> {
    let socket = {
        MAIN_SOCKET.lock().unwrap().as_ref().cloned()
    };
    let socket = if let Some(s) = socket {
        s
    } else {
        let raw = UdpSocket::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            PORT,
        ))
        .await?;
        raw.set_broadcast(true)?;
        Arc::new(raw)
    };
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let extra = build_user_extra();
    let packet = Packet {
        version: VER,
        packet_no: now_millis(),
        username,
        hostname,
        command: IPMSG_BR_EXIT,
        extra,
    };
    socket.send_to(&packet.encode(), to).await?;
    info!("BR_EXIT sent id={} to {}", packet.packet_no, to);
    Ok(())
}

/// Send a text message; returns the packet_no used, which the peer echoes
/// back in RECVMSG so the caller can track delivery status.
pub async fn send_message(to: SocketAddr, text: String) -> Result<u32> {
    let socket = {
        MAIN_SOCKET.lock().unwrap().as_ref().cloned()
    };
    let socket = if let Some(s) = socket {
        s
    } else {
        let raw = UdpSocket::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            PORT,
        ))
        .await?;
        raw.set_broadcast(true)?;
        Arc::new(raw)
    };
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let packet = Packet {
        version: VER,
        packet_no: now_millis(),
        username,
        hostname,
        command: IPMSG_SENDMSG | IPMSG_SENDCHECKOPT,
        extra: text,
    };
    socket.send_to(&packet.encode(), to).await?;
    info!(
        "SENDMSG sent id={} to {} text='{}'",
        packet.packet_no, to, packet.extra
    );
    Ok(packet.packet_no)
}

pub async fn send_file(to: SocketAddr, path: String) -> Result<()> {
    let socket = {
        MAIN_SOCKET.lock().unwrap().as_ref().cloned()
    };
    let socket = if let Some(s) = socket {
        s
    } else {
        let raw = UdpSocket::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            PORT,
        ))
        .await?;
        raw.set_broadcast(true)?;
        Arc::new(raw)
    };
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let meta = fs::metadata(&path).await?;
    let file_size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let file_name = PathBuf::from(&path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .replace(':', "::");
    let file_id = loop {
        let id = random_file_id();
        if id != 0 {
            break id;
        }
    };
    let packet_no = now_millis();
    {
        let mut table = FILE_TABLE.lock().unwrap();
        if table.contains_key(&(packet_no, file_id)) {
            return Err(anyhow!("file id collision, retry transfer"));
        }
        table.insert(
            (packet_no, file_id),
            FileEntry {
                to: to.ip(),
                path: PathBuf::from(&path),
                size: file_size,
                is_dir: false,
            },
        );
    }
    let size_hex = format!("{:x}", file_size);
    let mtime_hex = format!("{:x}", mtime);
    let attr_hex = format!("{:x}", IPMSG_FILE_REGULAR);
    // Per spec, only size/mtime/fileattr are hex; fileID is decimal
    // (protocol.txt §5, draft 14).
    let file_info = format!(
        "{}:{}:{}:{}:{}:\x07",
        file_id, file_name, size_hex, mtime_hex, attr_hex
    );
    let mut extra = String::new();
    extra.push_str("");
    extra.push('\0');
    extra.push_str(&file_info);
    let packet = Packet {
        version: VER,
        packet_no,
        username,
        hostname,
        command: IPMSG_SENDMSG | IPMSG_SENDCHECKOPT | IPMSG_FILEATTACHOPT,
        extra,
    };
    socket.send_to(&packet.encode(), to).await?;
    info!(
        "SENDMSG(FILE) sent id={} to {} file='{}' size={}",
        packet.packet_no, to, path, file_size
    );
    Ok(())
}

pub async fn send_files(to: SocketAddr, paths: Vec<String>) -> Result<Vec<SentFileMeta>> {
    let socket = {
        MAIN_SOCKET.lock().unwrap().as_ref().cloned()
    };
    let socket = if let Some(s) = socket {
        s
    } else {
        let raw = UdpSocket::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            PORT,
        ))
        .await?;
        raw.set_broadcast(true)?;
        Arc::new(raw)
    };
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };

    let mut valid_files = Vec::new();
    for path in paths {
         if let Ok(meta) = fs::metadata(&path).await {
             if meta.is_file() {
                 valid_files.push((path, meta));
             }
         }
    }
    
    if valid_files.is_empty() {
        return Err(anyhow!("No valid files to send"));
    }

    let packet_no = now_millis();
    let mut file_infos = String::new();
    let mut sent_items = Vec::new();

    {
        let mut table = FILE_TABLE.lock().unwrap();
        for (path, meta) in valid_files.iter() {
            let file_size = meta.len();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let file_name = PathBuf::from(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .replace(':', "::");

            let file_id = loop {
                let id = random_file_id();
                if id != 0 && !table.contains_key(&(packet_no, id)) {
                    break id;
                }
            };

            table.insert(
                (packet_no, file_id),
                FileEntry {
                    to: to.ip(),
                    path: PathBuf::from(path),
                    size: file_size,
                    is_dir: false,
                },
            );
            sent_items.push(SentFileMeta {
                packet_no,
                file_id,
                path: path.clone(),
                name: file_name.clone(),
                size: file_size,
                is_dir: false,
            });

            let size_hex = format!("{:x}", file_size);
            let mtime_hex = format!("{:x}", mtime);
            let attr_hex = format!("{:x}", IPMSG_FILE_REGULAR);
            let info = format!(
                "{}:{}:{}:{}:{}:\x07",
                file_id, file_name, size_hex, mtime_hex, attr_hex
            );
            file_infos.push_str(&info);
        }
    }

    let mut extra = String::new();
    extra.push_str("");
    extra.push('\0');
    extra.push_str(&file_infos);

    let packet = Packet {
        version: VER,
        packet_no,
        username,
        hostname,
        command: IPMSG_SENDMSG | IPMSG_SENDCHECKOPT | IPMSG_FILEATTACHOPT,
        extra,
    };
    socket.send_to(&packet.encode(), to).await?;
    info!(
        "SENDMSG(FILES) sent id={} to {} count={}",
        packet.packet_no, to, valid_files.len()
    );
    Ok(sent_items)
}

pub async fn send_folder(to: SocketAddr, dir: String) -> Result<SentFileMeta> {
    let socket = {
        MAIN_SOCKET.lock().unwrap().as_ref().cloned()
    };
    let socket = if let Some(s) = socket {
        s
    } else {
        let raw = UdpSocket::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            PORT,
        ))
        .await?;
        raw.set_broadcast(true)?;
        Arc::new(raw)
    };
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let meta = fs::metadata(&dir).await?;
    if !meta.is_dir() {
        return Err(anyhow!("send_folder path is not directory"));
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir_name = PathBuf::from(&dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("dir")
        .replace(':', "::");
    let file_id = loop {
        let id = random_file_id();
        if id != 0 {
            break id;
        }
    };
    let packet_no = now_millis();
    {
        let mut table = FILE_TABLE.lock().unwrap();
        if table.contains_key(&(packet_no, file_id)) {
            return Err(anyhow!("file id collision, retry transfer"));
        }
        table.insert(
            (packet_no, file_id),
            FileEntry {
                to: to.ip(),
                path: PathBuf::from(&dir),
                size: 0,
                is_dir: true,
            },
        );
    }
    let size_hex = format!("{:x}", 0u64);
    let mtime_hex = format!("{:x}", mtime);
    let attr_hex = format!("{:x}", IPMSG_FILE_DIR);
    let file_info = format!(
        "{}:{}:{}:{}:{}:\x07",
        file_id, dir_name, size_hex, mtime_hex, attr_hex
    );
    let text = format!("[文件夹] {}", dir_name);
    let mut extra = String::new();
    extra.push_str(&text);
    extra.push('\0');
    extra.push_str(&file_info);
    let packet = Packet {
        version: VER,
        packet_no,
        username,
        hostname,
        command: IPMSG_SENDMSG | IPMSG_SENDCHECKOPT | IPMSG_FILEATTACHOPT,
        extra,
    };
    socket.send_to(&packet.encode(), to).await?;
    info!(
        "SENDMSG(DIR) sent id={} to {} dir='{}'",
        packet.packet_no, to, dir
    );
    Ok(SentFileMeta {
        packet_no,
        file_id,
        path: dir,
        name: dir_name,
        size: 0,
        is_dir: true,
    })
}

async fn recv_file_once<F>(
    from: SocketAddr,
    packet_no: u32,
    file_id: u32,
    save_path: &str,
    use_hex: bool,
    expected_size: u64,
    on_progress: &mut F,
) -> Result<u64>
where
    F: FnMut(u64),
{
    let mut stream = TcpStream::connect(from).await?;
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    let extra = if use_hex {
        format!("{:x}:{:x}:0", packet_no, file_id)
    } else {
        format!("{}:{}:0", packet_no, file_id)
    };
    let packet = Packet {
        version: VER,
        packet_no: now_millis(),
        username,
        hostname,
        command: IPMSG_GETFILEDATA,
        extra,
    };
    let buf = packet.encode();
    stream.write_all(&buf).await?;
    let mut file = fs::File::create(save_path).await?;
    let mut buf = [0u8; 8192];
    let mut total: u64 = 0;
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).await?;
        total += n as u64;
        on_progress(total);
        if expected_size > 0 && total >= expected_size {
            break;
        }
    }
    info!(
        "RECVFILE attempt mode={} from {} packet_no={} (0x{:x}) file_id={} (0x{:x}) size={} path='{}'",
        if use_hex { "hex" } else { "dec" },
        from,
        packet_no,
        packet_no,
        file_id,
        file_id,
        total,
        save_path
    );
    if expected_size > 0 && total < expected_size {
        return Err(anyhow!(
            "incomplete file transfer: expected {} bytes, got {}",
            expected_size,
            total
        ));
    }
    Ok(total)
}

pub async fn recv_file<F>(
    from: SocketAddr,
    packet_no: u32,
    file_id: u32,
    expected_size: u64,
    save_path: String,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64) + Send,
{
    info!("recv_file start from {} packet_no={} file_id={}", from, packet_no, file_id);
    let first_try = recv_file_once(
        from,
        packet_no,
        file_id,
        &save_path,
        true, // use hex
        expected_size,
        &mut on_progress,
    )
    .await;
    if first_try.is_err() {
        info!(
            "recv_file retry with decimal mode from {} packet_no={} file_id={}",
            from, packet_no, file_id
        );
        recv_file_once(
            from,
            packet_no,
            file_id,
            &save_path,
            false, // retry with decimal
            expected_size,
            &mut on_progress,
        )
        .await?;
    }
    Ok(())
}

pub async fn recv_folder<F>(
    from: SocketAddr,
    packet_no: u32,
    file_id: u32,
    save_path: String,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, String) + Send,
{
    info!("recv_folder start from {} packet_no={} file_id={}", from, packet_no, file_id);
    let mut stream = TcpStream::connect(from).await?;
    let (username, hostname) = {
        let info = USER_INFO.read().unwrap();
        (info.username.clone(), info.hostname.clone())
    };
    // For folder, extra format is packet_no:file_id:0 (same as file?)
    // Actually spec says just packet_no:file_id. The 3rd field is offset, usually 0.
    let extra = format!("{:x}:{:x}:0", packet_no, file_id);
    
    let packet = Packet {
        version: VER,
        packet_no: now_millis(),
        username,
        hostname,
        command: IPMSG_GETDIRFILES,
        extra,
    };
    let buf = packet.encode();
    stream.write_all(&buf).await?;

    let mut path_stack = vec![PathBuf::from(&save_path)];
    // Ensure the root save_path exists (it should be created by the caller? No, we are creating the folder structure inside it? 
    // Wait, save_path INCLUDES the folder name. So we should create it.
    if !std::path::Path::new(&save_path).exists() {
        fs::create_dir_all(&save_path).await?;
    }

    let mut total_received: u64 = 0;
    let mut reader = tokio::io::BufReader::new(stream);
    let mut is_first_entry = true;

    loop {
        // Read header size (hex string until ':')
        let mut size_buf = Vec::new();
        loop {
            let mut b = [0u8; 1];
            let n = reader.read(&mut b).await?;
            if n == 0 {
                // End of stream. If no entry was received, treat it as canceled/unavailable.
                if is_first_entry {
                    return Err(anyhow!("no folder data received"));
                }
                return Ok(());
            }
            if b[0] == b':' {
                break;
            }
            size_buf.push(b[0]);
            if size_buf.len() > 10 { // Safety limit
                 return Err(anyhow!("Header size too long"));
            }
        }

        let size_str = String::from_utf8_lossy(&size_buf);
        let header_len = u32::from_str_radix(&size_str, 16)?;
        
        // 2. Read the rest of the header
        // We already read size_buf.len() + 1 bytes
        let remaining_header_len = header_len as usize - size_buf.len() - 1;
        let mut header_buf = vec![0u8; remaining_header_len];
        reader.read_exact(&mut header_buf).await?;
        
        // Parse header: filename:size:type:attrs...
        // Decode header string (GB18030 usually)
        let header_str = decode_text(&header_buf);
        let parts: Vec<&str> = header_str.split(':').collect();
        
        if parts.len() < 3 {
             return Err(anyhow!("Invalid folder header"));
        }
        
        // Handle potential leading colon (which results in empty first part)
        let (filename, file_size_str, file_type_str) = if parts[0].is_empty() && parts.len() >= 4 {
             (parts[1], parts[2], parts[3])
        } else if !parts[0].is_empty() && parts.len() >= 3 {
             (parts[0], parts[1], parts[2])
        } else {
             return Err(anyhow!("Invalid folder header: {:?}", header_str));
        };
        
        let file_size = u64::from_str_radix(file_size_str, 16).map_err(|e| anyhow!("Invalid file size: {} ({})", file_size_str, e))?;
        let file_type = u32::from_str_radix(file_type_str, 16).map_err(|e| anyhow!("Invalid file type: {} ({})", file_type_str, e))?;
        
        let current_is_first = is_first_entry;
        is_first_entry = false;

        match file_type {
            IPMSG_FILE_REGULAR => {
                let Some(filename) = sanitize_wire_filename(filename) else {
                    return Err(anyhow!(
                        "refused unsafe filename in folder transfer: {:?}",
                        header_str
                    ));
                };
                if let Some(current_dir) = path_stack.last() {
                    let file_path = current_dir.join(filename);
                    let mut file = fs::File::create(&file_path).await?;
                    // Notify UI immediately when a new file header is parsed,
                    // so filename switches without waiting for first data chunk.
                    on_progress(total_received, filename.to_string());
                    
                    // Read file content
                    let mut received = 0u64;
                    let mut buf = [0u8; 8192];
                    while received < file_size {
                        let to_read = std::cmp::min(buf.len() as u64, file_size - received) as usize;
                        let n = reader.read(&mut buf[..to_read]).await?;
                        if n == 0 {
                            return Err(anyhow!("Unexpected EOF during file content"));
                        }
                        file.write_all(&buf[..n]).await?;
                        received += n as u64;
                        total_received += n as u64;
                        on_progress(total_received, filename.to_string());
                    }
                }
            }
            IPMSG_FILE_DIR => {
                if current_is_first {
                    // IPMSG protocol sends the root directory header first.
                    // Since 'save_path' already includes the folder name (e.g. .../Downloads/FolderName),
                    // we treat this first header as the root directory itself, not a subdirectory.
                    // We simply skip creating a new directory level to avoid double nesting (FolderName/FolderName).
                    // This effectively maps the sender's root folder name to our local 'save_path'.
                    continue;
                }
                let Some(filename) = sanitize_wire_filename(filename) else {
                    return Err(anyhow!(
                        "refused unsafe directory name in folder transfer: {:?}",
                        header_str
                    ));
                };
                if let Some(current_dir) = path_stack.last() {
                    let dir_path = current_dir.join(filename);
                    if !dir_path.exists() {
                        fs::create_dir(&dir_path).await?;
                    }
                    path_stack.push(dir_path);
                }
            }
            IPMSG_FILE_RETPARENT => {
                if filename == "." {
                   // Some clients send "." for RETPARENT?
                   // Just pop
                }
                path_stack.pop();
            }
            _ => {
                // Ignore other types or handle if needed
                // If it has content size, we must skip it
                if file_size > 0 {
                    let mut skipped = 0u64;
                    let mut buf = [0u8; 8192];
                    while skipped < file_size {
                        let to_read = std::cmp::min(buf.len() as u64, file_size - skipped) as usize;
                        let n = reader.read(&mut buf[..to_read]).await?;
                        if n == 0 { break; }
                        skipped += n as u64;
                    }
                }
            }
        }
    }
}

async fn write_dir_header(
    stream: &mut TcpStream,
    filename: &str,
    file_attr: u32,
    file_size: u64,
    mtime: u64,
) -> Result<()> {
    let ts = if mtime == 0 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs())
            .unwrap_or(0)
    } else {
        mtime
    };
    let mut header = String::new();
    header.push(':');
    header.push_str(filename);
    header.push(':');
    header.push_str(&format!("{:x}", file_size));
    header.push(':');
    header.push_str(&format!("{:x}", file_attr));
    header.push(':');
    header.push_str(&format!("{:x}={:x}", IPMSG_FILE_CREATETIME, ts));
    header.push(':');
    header.push_str(&format!("{:x}={:x}", IPMSG_FILE_MTIME, ts));
    header.push(':');
    let header_bytes = encode_text(&header);
    let header_len = header_bytes.len();
    let total_len = header_len + 4;
    let len_hex = format!("{:0>4x}", total_len);
    let mut out = len_hex.into_bytes();
    out.extend_from_slice(&header_bytes);
    stream.write_all(&out).await?;
    Ok(())
}

fn send_dir_children<'a>(
    stream: &'a mut TcpStream,
    dir: PathBuf,
    cancel_token: Arc<AtomicBool>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if cancel_token.load(Ordering::SeqCst) {
                return Err(anyhow!("send canceled"));
            }
            let path = entry.path();
            let meta = entry.metadata().await?;
            if meta.is_dir() {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dir");
                write_dir_header(stream, name, IPMSG_FILE_DIR, 0, mtime).await?;
                send_dir_children(stream, path.clone(), cancel_token.clone()).await?;
                write_dir_header(stream, ".", IPMSG_FILE_RETPARENT, 0, 0).await?;
            } else if meta.is_file() {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file");
                // Never emit entries whose name we would refuse on the receive
                // side (e.g. NTFS alternate data streams like "f.txt:ads").
                if sanitize_wire_filename(name).is_none() {
                    warn!("skip sending entry with unsafe name: {:?}", path);
                    continue;
                }
                let size = meta.len();
                write_dir_header(stream, name, IPMSG_FILE_REGULAR, size, mtime).await?;
                let mut file = fs::File::open(&path).await?;
                let mut buf = [0u8; 8192];
                let mut sent: u64 = 0;
                loop {
                    if cancel_token.load(Ordering::SeqCst) {
                        return Err(anyhow!("send canceled"));
                    }
                    let n = file.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    stream.write_all(&buf[..n]).await?;
                    sent += n as u64;
                    if sent >= size {
                        break;
                    }
                }
            }
        }
        Ok(())
    })
}

async fn send_dir_hierarchy(
    stream: &mut TcpStream,
    root: &PathBuf,
    cancel_token: Arc<AtomicBool>,
) -> Result<()> {
    if cancel_token.load(Ordering::SeqCst) {
        return Err(anyhow!("send canceled"));
    }
    let root_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("dir");
    let meta = fs::metadata(root).await?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    write_dir_header(stream, root_name, IPMSG_FILE_DIR, 0, mtime).await?;
    send_dir_children(stream, root.clone(), cancel_token.clone()).await?;
    write_dir_header(stream, ".", IPMSG_FILE_RETPARENT, 0, 0).await?;
    Ok(())
}

/// Peers echo packet/file ids in GETFILEDATA/GETDIRFILES with different
/// radices: the spec mandates hex, but several Chinese clients (飞鸽/飞秋等)
/// use decimal. Resolve against the file table instead of trusting one radix.
fn lookup_file_entry(
    table: &HashMap<(u32, u32), FileEntry>,
    pkt_no: &str,
    file_id: &str,
) -> Option<(u32, u32, FileEntry)> {
    let mut candidates: Vec<Vec<u32>> = Vec::with_capacity(2);
    for s in [pkt_no, file_id] {
        let s = s.trim();
        let mut vals: Vec<u32> = Vec::new();
        if let Ok(v) = u32::from_str_radix(s, 16) {
            vals.push(v);
        }
        if let Ok(v) = s.parse::<u32>()
            && !vals.contains(&v)
        {
            vals.push(v);
        }
        let auto = parse_u32_auto_radix(s);
        if !vals.contains(&auto) {
            vals.push(auto);
        }
        candidates.push(vals);
    }
    for a in &candidates[0] {
        for b in &candidates[1] {
            if let Some(entry) = table.get(&(*a, *b)) {
                return Some((*a, *b, entry.clone()));
            }
        }
    }
    None
}

async fn handle_tcp_file(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    tx: broadcast::Sender<Event>,
) -> Result<()> {
    info!("handle_tcp_file");
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let header = &buf[..n];
    let packet = parse_packet(header)?;
    let base = packet.command & 0x000000ff;
    if base == IPMSG_GETFILEDATA {
        let parts: Vec<&str> = packet.extra.split(':').collect();
        if parts.len() < 3 {
            return Err(anyhow!("bad GETFILEDATA extra"));
        }
        let (pkt_no, file_id, entry) = {
            let table = FILE_TABLE.lock().unwrap();
            info!("handle_tcp_file table={:?}", table);
            match lookup_file_entry(&table, parts[0], parts[1]) {
                Some((p, f, e)) => (p, f, Some(e)),
                None => (0, 0, None),
            }
        };
        let offset = parse_u64_auto_radix(parts[2].trim());
        info!("handle_tcp_file packet_no={:x} file_id={:x} offset={:x} {:?}", pkt_no, file_id, offset, entry);
        if let Some(entry) = entry {
            // Only the peer the file was offered to may download it (P0-3).
            // Compare by IP: the TCP client connects from an ephemeral port,
            // so full socket equality never matches.
            if entry.to != peer_addr.ip() {
                warn!(
                    "denied GETFILEDATA ({:x},{:x}) from {}: not intended recipient {:?}",
                    pkt_no, file_id, peer_addr.ip(), entry.to
                );
                return Err(anyhow!("file request not authorized"));
            }
            if entry.is_dir {
                return Ok(());
            }
            let cancel_token = send_cancel_token(peer_addr.ip(), pkt_no, file_id);
            if cancel_token.load(Ordering::SeqCst) {
                clear_send_cancel_token(peer_addr.ip(), pkt_no, file_id);
                return Err(anyhow!("send canceled"));
            }
            let _ = tx.send(Event::FileServingStarted {
                to: peer_addr,
                packet_no: pkt_no,
                file_id,
                is_dir: false,
            });
            let mut file = fs::File::open(&entry.path).await?;
            if offset > 0 {
                file.seek(std::io::SeekFrom::Start(offset)).await?;
            }
            let mut buf = [0u8; 8192];
            let send_result: Result<()> = async {
                loop {
                    if cancel_token.load(Ordering::SeqCst) {
                        return Err(anyhow!("send canceled"));
                    }
                    let n = file.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    stream.write_all(&buf[..n]).await?;
                }
                Ok(())
            }
            .await;
            if let Err(err) = send_result {
                clear_send_cancel_token(peer_addr.ip(), pkt_no, file_id);
                return Err(err);
            }
            info!(
                "SENDFILE done packet_no={:x} file_id={:x} path='{}'",
                pkt_no,
                file_id,
                entry.path.to_string_lossy()
            );
            let _ = tx.send(Event::FileServed {
                to: peer_addr,
                packet_no: pkt_no,
                file_id,
                is_dir: false,
            });
            clear_send_cancel_token(peer_addr.ip(), pkt_no, file_id);
        }
        return Ok(());
    }
    if base == IPMSG_GETDIRFILES {
        let parts: Vec<&str> = packet.extra.split(':').collect();
        if parts.len() < 2 {
            return Err(anyhow!("bad GETDIRFILES extra"));
        }
        let (pkt_no, file_id, entry) = {
            let table = FILE_TABLE.lock().unwrap();
            match lookup_file_entry(&table, parts[0], parts[1]) {
                Some((p, f, e)) => (p, f, Some(e)),
                None => (0, 0, None),
            }
        };
        let _offset = if parts.len() >= 3 {
            parse_u64_auto_radix(parts[2].trim())
        } else {
            0
        };
        if let Some(entry) = entry {
            // Only the peer the file was offered to may download it (P0-3).
            if entry.to != peer_addr.ip() {
                warn!(
                    "denied GETDIRFILES ({:x},{:x}) from {}: not intended recipient {:?}",
                    pkt_no, file_id, peer_addr.ip(), entry.to
                );
                return Err(anyhow!("file request not authorized"));
            }
            if entry.is_dir {
                let cancel_token = send_cancel_token(peer_addr.ip(), pkt_no, file_id);
                if cancel_token.load(Ordering::SeqCst) {
                    clear_send_cancel_token(peer_addr.ip(), pkt_no, file_id);
                    return Err(anyhow!("send canceled"));
                }
                let _ = tx.send(Event::FileServingStarted {
                    to: peer_addr,
                    packet_no: pkt_no,
                    file_id,
                    is_dir: true,
                });
                let send_result =
                    send_dir_hierarchy(&mut stream, &entry.path, cancel_token.clone()).await;
                if let Err(err) = send_result {
                    clear_send_cancel_token(peer_addr.ip(), pkt_no, file_id);
                    return Err(err);
                }
                info!(
                    "SENDDIR done packet_no={:x} file_id={:x} path='{}'",
                    pkt_no,
                    file_id,
                    entry.path.to_string_lossy()
                );
                let _ = tx.send(Event::FileServed {
                    to: peer_addr,
                    packet_no: pkt_no,
                    file_id,
                    is_dir: true,
                });
                clear_send_cancel_token(peer_addr.ip(), pkt_no, file_id);
            }
        }
    }
    Ok(())
}


pub async fn start_ipmsg() -> Result<(broadcast::Receiver<Event>, u16)> {
    env_logger::try_init().ok();
    let service = Service::new().await?;
    let rx = service.events.subscribe();
    let port = service.port;
    service.spawn().await?;
    Ok((rx, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Peers (original IPMsg and clones) parse wire ids with *signed* 32-bit
    /// arithmetic. Regression guard for the "fileid兼容32位" constraint.
    #[test]
    fn random_file_id_stays_in_signed_32bit_range() {
        for _ in 0..10_000 {
            let id = random_file_id();
            assert_ne!(id, 0);
            assert!(
                id < 0x8000_0000,
                "file_id {id:#x} overflows signed 32-bit parse on peers"
            );
        }
    }

    #[test]
    fn sanitize_rejects_path_traversal_and_device_names() {
        assert!(sanitize_wire_filename("..\\..\\evil.exe").is_none());
        assert!(sanitize_wire_filename("../../evil").is_none());
        assert!(sanitize_wire_filename("C:\\evil").is_none());
        assert!(sanitize_wire_filename("f.txt:ads").is_none());
        assert!(sanitize_wire_filename("..").is_none());
        assert!(sanitize_wire_filename("").is_none());
        assert!(sanitize_wire_filename("CON").is_none());
        assert_eq!(sanitize_wire_filename("normal.txt"), Some("normal.txt"));
        assert_eq!(sanitize_wire_filename("中文文件.pdf"), Some("中文文件.pdf"));
    }

    /// GETFILEDATA/GETDIRFILES ids arrive in hex (spec) or decimal (several
    /// Chinese clients). The lookup must resolve either against the table.
    #[test]
    fn lookup_file_entry_resolves_hex_and_decimal_echo() {
        let mut table = HashMap::new();
        table.insert(
            (34, 271_038_928),
            FileEntry {
                to: "192.168.68.245".parse().unwrap(),
                path: PathBuf::from("bevy_game_1.zip"),
                size: 45_000_000,
                is_dir: false,
            },
        );
        // Spec-compliant peer echoes hex: "22:1027b9d0"
        let hit = lookup_file_entry(&table, "22", "1027b9d0").expect("hex echo must resolve");
        assert_eq!(hit.0, 34);
        assert_eq!(hit.1, 271_038_928);
        // Decimal echo: "34:271038928"
        let hit = lookup_file_entry(&table, "34", "271038928").expect("decimal echo must resolve");
        assert_eq!(hit.0, 34);
        assert_eq!(hit.1, 271_038_928);
        // Unknown ids must not resolve
        assert!(lookup_file_entry(&table, "99", "999").is_none());
    }
}
