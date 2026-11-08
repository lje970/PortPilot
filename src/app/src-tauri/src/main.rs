// PortPilot 0.2.0 · Tauri 后端(P1)
// ==================================
// 多标签会话(HashMap<tab_id, SessionEntry>)、自动应答规则引擎、
// 示波器数据通路(ScopeParser 抽头)、CRC 发送。
// 自动化冒烟不变:portpilot-app.exe --port COM9 --baud 460800 --seconds 12 --out smoke.jsonl

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use portpilot_core as core;
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager, State};

// ---------- 探针(CLI 冒烟) ----------
struct Probe {
    file: Option<File>,
    deadline: Option<Instant>,
}

// ---------- 规则(自动应答) ----------
#[derive(Clone, Serialize)]
pub struct RuleOut {
    pub enabled: bool,
    pub kind: String,   // text | hex | regex
    pub pattern: String,
    pub payload: String,
    pub hex: bool,
    pub newline: String,
}

struct Rule {
    enabled: bool,
    kind: String,
    pattern: String,
    test_re: Option<Regex>,
    hex_pattern: Vec<u8>,
    hex: bool,
    payload_bytes: Vec<u8>, // 编码在触发时按会话编码进行(文本);HEX 预解析
    payload_hex: Vec<u8>,
    newline: String,
    last_fire: Option<Instant>,
}

const RULE_COOLDOWN_MS: u128 = 300;

impl Rule {
    fn from_input(r: RuleIn, enc: u8) -> Self {
        let test_re = if r.kind == "regex" {
            Regex::new(&r.pattern).ok()
        } else {
            None
        };
        let hex_pattern = if r.kind == "hex" {
            hex_parse(&r.pattern)
        } else {
            Vec::new()
        };
        let payload_hex = if r.hex { hex_parse(&r.payload) } else { Vec::new() };
        let mut payload_bytes = if r.hex { Vec::new() } else { encode_text(&r.payload, enc) };
        match r.newline.as_str() {
            "cr" => payload_bytes.push(b'\r'),
            "lf" => payload_bytes.push(b'\n'),
            "crlf" => payload_bytes.extend_from_slice(b"\r\n"),
            _ => {}
        }
        Rule {
            enabled: r.enabled,
            kind: r.kind,
            pattern: r.pattern,
            test_re,
            hex_pattern,
            hex: r.hex,
            payload_bytes,
            payload_hex,
            newline: r.newline,
            last_fire: None,
        }
    }

    /// 返回 Some(待发送字节) 表示命中且过了冷却。
    fn eval(&mut self, raw: &[u8], text: &str) -> Option<Vec<u8>> {
        if !self.enabled {
            return None;
        }
        if let Some(t) = self.last_fire {
            if t.elapsed().as_millis() < RULE_COOLDOWN_MS {
                return None;
            }
        }
        let hit = match self.kind.as_str() {
            "text" => text.to_lowercase().contains(&self.pattern.to_lowercase()),
            "hex" => !self.hex_pattern.is_empty()
                && raw
                    .windows(self.hex_pattern.len())
                    .any(|w| w == self.hex_pattern.as_slice()),
            "regex" => self.test_re.as_ref().map(|r| r.is_match(text)).unwrap_or(false),
            _ => false,
        };
        if hit {
            self.last_fire = Some(Instant::now());
            Some(if self.hex {
                self.payload_hex.clone()
            } else {
                self.payload_bytes.clone()
            })
        } else {
            None
        }
    }
}

#[derive(serde::Deserialize)]
pub struct RuleIn {
    pub enabled: bool,
    pub kind: String,
    pub pattern: String,
    pub payload: String,
    pub hex: bool,
    pub newline: String,
}

// ---------- 会话条目 ----------
struct SessionEntry {
    handle: core::SessionHandle,
    stop: Arc<AtomicBool>,
    log_path: PathBuf,
    encoding: Arc<AtomicU8>,
    scope_parser: Arc<Mutex<Box<dyn core::ScopeParser>>>,
    decoder: Arc<Mutex<Option<core::FrameDecoder>>>,
    port: String,
    baud: u32,
}

struct AppState {
    sessions: Mutex<HashMap<String, SessionEntry>>,
    rules: Arc<Mutex<Vec<Rule>>>,
    scope_on: Arc<AtomicBool>,
    scope_channels: Arc<AtomicU8>,
    scope_protocol: Mutex<String>,
    probe: Arc<Mutex<Probe>>,
    /// 日志回放停止标志:tab -> flag(回放不依赖会话)
    replays: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Lua 脚本停止标志:tab -> flag
    scripts: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

// ---------- 事件与工具 ----------
#[derive(Serialize, Clone)]
struct LineOut { ts: String, s: String }

#[derive(Serialize, Clone)]
struct RxEvent {
    tab: String,
    seq: u64,
    lines: Vec<LineOut>,
    hex: String,
    bytes: usize,
    total_rx: u64,
    connected: bool,
    reconnects: u32,
}

#[derive(Serialize, Clone)]
struct ScopeEvent { tab: String, frames: Vec<Vec<f32>> }

#[derive(Serialize, Clone)]
struct PortOut { name: String, usb: String }

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else if (c as u32) < 32 && c != '\t' { '·' } else { c })
        .collect()
}

fn decode_bytes(raw: &[u8], enc: u8) -> String {
    let (cow, _, _) = if enc == 1 { encoding_rs::GBK.decode(raw) } else { encoding_rs::UTF_8.decode(raw) };
    cow.into_owned()
}

fn encode_text(s: &str, enc: u8) -> Vec<u8> {
    if enc == 1 { encoding_rs::GBK.encode(s).0.into_owned() } else { s.as_bytes().to_vec() }
}

fn hex_parse(s: &str) -> Vec<u8> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if cleaned.len() % 2 != 0 {
        return Vec::new();
    }
    (0..cleaned.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok())
        .collect()
}

fn wall_ts(epoch_ms_at_start: u64, arrival_ms: f64) -> String {
    let ms = epoch_ms_at_start + arrival_ms as u64;
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|d| d.with_timezone(&chrono::Local))
        .unwrap_or_else(chrono::Local::now)
        .format("%H:%M:%S%.3f")
        .to_string()
}

fn now_epoch_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn hex_str(data: &[u8], cap: usize) -> String {
    let n = data.len().min(cap);
    let mut s = String::with_capacity(n * 3);
    for (i, b) in data[..n].iter().enumerate() {
        if i > 0 { s.push(' '); }
        s.push_str(&format!("{:02x}", b));
    }
    if data.len() > cap { s.push_str(" …"); }
    s
}

// ---------- 命令 ----------
#[tauri::command]
fn list_ports() -> Vec<PortOut> {
    core::list_ports()
        .into_iter()
        .map(|p| PortOut {
            name: p.name.clone(),
            usb: match (p.usb_vid, p.usb_pid) {
                (Some(v), Some(pid)) => format!("{:04X}:{:04X}", v, pid),
                _ => String::new(),
            },
        })
        .collect()
}

#[tauri::command]
fn connect(
    tab: String,
    port: String,
    baud: u32,
    encoding: String,
    kind: String,
    remote: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    {
        // 已有旧会话(含重连循环卡住/端口占用的"僵尸会话")→ 停旧起新,不再报错困死用户
        let mut sessions = state.sessions.lock().unwrap();
        if let Some(old) = sessions.remove(&tab) {
            old.stop.store(true, Ordering::Relaxed);
            old.handle.stop(); // 释放串口(同坑 19)
        }
    }
    let log_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("logs")))
        .unwrap_or_else(|| PathBuf::from("logs"));
    let _ = std::fs::create_dir_all(&log_dir);
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S%.3f");
    let log_path = log_dir.join(format!("PortPilot_{}_{}.bin", tab, stamp));
    // pplog v1:自描述会话日志(ms 时间戳+方向),回放可忠实重放、离线可重解析(见 docs/03 §8.11)
    let log_meta = core::pplog::PplogMeta {
        kind: kind.clone(),
        port: port.clone(),
        baud,
        encoding: encoding.clone(),
        created_utc: chrono::Utc::now().to_rfc3339(),
    };
    let log_file =
        core::pplog::PplogWriter::create(&log_path, &log_meta).map_err(|e| format!("创建日志失败: {}", e))?;

    let enc_val = if encoding == "gbk" { 1u8 } else { 0u8 };
    let encoding_a8 = Arc::new(AtomicU8::new(enc_val));
    let scope_parser: Arc<Mutex<Box<dyn core::ScopeParser>>> = Arc::new(Mutex::new(
        core::make_parser(
            &state.scope_protocol.lock().unwrap().clone(),
            state.scope_channels.load(Ordering::Relaxed) as usize,
        ),
    ));
    let decoder: Arc<Mutex<Option<core::FrameDecoder>>> = Arc::new(Mutex::new(None));

    let port_for_factory = port.clone();
    let remote_for_factory = remote.clone();
    let kind_for_factory = kind.clone();
    let tkind = core::TransportKind::from_str(&kind);
    let session = core::spawn_session(core::SessionOptions::new(Box::new(move || {
        // RTT:走 portpilot-rtt 的专用传输(port 字段 = 芯片名,如 esp32s3);
        // 工厂闭包内重建 = 断线重连自动重 attach + 重扫描(约 1s)
        if kind_for_factory == "rtt" {
            return Ok(Box::new(portpilot_rtt::RttTransport::open(&port_for_factory)?)
                as Box<dyn core::transport::Transport>);
        }
        let cfg = core::TransportConfig {
            kind: tkind,
            port: port_for_factory.clone(),
            remote: remote_for_factory.clone(),
            baud,
            data_bits: 8,
            stop_bits: 1,
            parity: core::Parity::None,
            read_timeout_ms: 5,
        };
        core::open_transport(&cfg)
    })));

    // ---- 泵线程 ----
    let stop = Arc::new(AtomicBool::new(false));
    {
        let handle = app.clone();
        let stop2 = Arc::clone(&stop);
        let session2 = session.clone();
        let probe = Arc::clone(&state.probe);
        let rules = Arc::clone(&state.rules);
        let scope_on = Arc::clone(&state.scope_on);
        let scope_parser2 = Arc::clone(&scope_parser);
        let th_dec = Arc::clone(&decoder);
        let th_enc = Arc::clone(&encoding_a8);
        let dec_count = Arc::new(AtomicU64::new(0));
        let dec_n = Arc::clone(&dec_count);
        let tab2 = tab.clone();
        thread::spawn(move || {
            let mut log_file = log_file;
            let mut buf = vec![0u8; 256 * 1024];
            let mut pending: Vec<u8> = Vec::new();
            let mut pending_arrival = 0.0f64;
            let mut next_frame = Instant::now();
            let mut next_sec = Instant::now();
            let pump_start = Instant::now();
            let mut seq: u64 = 0;
            let epoch_ms = now_epoch_ms();
            loop {
                let now = Instant::now();
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                if now < next_frame {
                    thread::sleep(Duration::from_micros(400));
                    continue;
                }
                next_frame += Duration::from_millis(33);
                let rep = session2.drain(&mut buf);
                // 探针行(每秒;写在 bytes==0 早退之前——静默链路也要产出探针)
                if now >= next_sec {
                    next_sec += Duration::from_secs(1);
                    let mut p2 = probe.lock().unwrap();
                    if let Some(f) = p2.file.as_mut() {
                        let st = session2.stats();
                        let _ = writeln!(
                            f,
                            "{{\"t\": {}, \"rx_bytes\": {}, \"tx_bytes\": {}, \"connected\": {}, \"reconnects\": {}, \"backpressure\": {}, \"dec\": {}}}",
                            pump_start.elapsed().as_secs(),
                            st.rx_bytes, st.tx_bytes, st.connected, st.reconnects, st.backpressure_events,
                            dec_n.load(Ordering::Relaxed)
                        );
                    }
                }
                if rep.bytes == 0 {
                    continue;
                }
                let data = &buf[..rep.bytes];
                let _ = log_file.write_record(core::pplog::DIR_RX, data); // pplog 记录落盘(带 ms 时间戳)
                let arrival = rep.newest_arrival_ms.unwrap_or_else(|| session2.now_ms());

                // 行组装(按编码逐行解码)
                pending.extend_from_slice(data);
                let enc = th_enc.load(Ordering::Relaxed);
                let mut lines: Vec<LineOut> = Vec::new();
                let mut start = 0usize;
                for i in 0..pending.len() {
                    if pending[i] == b'\n' {
                        let raw = decode_bytes(&pending[start..i], enc);
                        lines.push(LineOut { ts: wall_ts(epoch_ms, pending_arrival), s: sanitize(&raw) });
                        start = i + 1;
                        pending_arrival = arrival;
                    }
                }
                pending.drain(..start);
                if pending.len() > 2048 {
                    let raw = decode_bytes(&pending, enc);
                    lines.push(LineOut { ts: wall_ts(epoch_ms, pending_arrival), s: sanitize(&raw) });
                    pending.clear();
                }

                // 自动应答规则
                {
                    let text = decode_bytes(data, enc);
                    let mut rules = rules.lock().unwrap();
                    let mut to_send: Vec<Vec<u8>> = Vec::new();
                    for r in rules.iter_mut() {
                        if let Some(bytes) = r.eval(data, &text) {
                            to_send.push(bytes);
                        }
                    }
                    drop(rules);
                    for bytes in to_send {
                        let _ = session2.write(&bytes);
                    }
                }

                // 示波器抽头(全部帧上送,前端自存缓冲;不做 19:1 抽稀——那会把正弦画成锯齿)
                if scope_on.load(Ordering::Relaxed) {
                    let frames = scope_parser2.lock().unwrap().feed(data);
                    if !frames.is_empty() {
                        let _ = handle.emit(
                            "scope",
                            ScopeEvent { tab: tab2.clone(), frames },
                        );
                    }
                }

                // 协议解码抽头(启用时逐批喂入,帧事件按标签路由)
                {
                    let mut d = th_dec.lock().unwrap();
                    if let Some(dec) = d.as_mut() {
                        let frames = dec.feed(data);
                        if !frames.is_empty() {
                            dec_n.fetch_add(frames.len() as u64, Ordering::Relaxed);
                            let _ = handle.emit(
                                "decoded",
                                serde_json::json!({ "tab": tab2.clone(), "frames": frames }),
                            );
                        }
                    }
                }

                let ev = RxEvent {
                    tab: tab2.clone(),
                    seq: seq + 1,
                    lines,
                    hex: hex_str(data, 2048),
                    bytes: rep.bytes,
                    total_rx: session2.stats().rx_bytes,
                    connected: session2.stats().connected,
                    reconnects: session2.stats().reconnects,
                };
                seq = ev.seq;
                let _ = handle.emit("rx", &ev);

            }
        });
    }

    let entry = SessionEntry {
        handle: session,
        stop,
        log_path: log_path.clone(),
        encoding: encoding_a8,
        scope_parser,
        decoder,
        port: port.clone(),
        baud,
    };
    state.sessions.lock().unwrap().insert(tab, entry);
    Ok(format!("已连接 {port} @ {baud};日志: {}", log_path.display()))
}

#[tauri::command]
fn disconnect(tab: String, state: State<'_, AppState>) -> Result<String, String> {
    let mut sessions = state.sessions.lock().unwrap();
    match sessions.remove(&tab) {
        Some(e) => {
            e.stop.store(true, Ordering::Relaxed); // 停泵线程
            e.handle.stop(); // 停会话 I/O 线程并释放串口(此前漏调,端口被永久占用,坑 19)
            let st = e.handle.stats();
            Ok(format!("已断开;RX={}B TX={}B 重连={}", st.rx_bytes, st.tx_bytes, st.reconnects))
        }
        None => Err("该标签没有会话".into()),
    }
}

#[tauri::command]
fn send(
    tab: String,
    data: String,
    hex_mode: bool,
    newline: String,
    checksum: String,
    endian: String,
    state: State<'_, AppState>,
) -> Result<u64, String> {
    let (enc, handle) = {
        let sessions = state.sessions.lock().unwrap();
        match sessions.get(&tab) {
            Some(e) => (e.encoding.load(Ordering::Relaxed), e.handle.clone()),
            None => return Err("该标签未连接".into()),
        }
    };
    let mut payload: Vec<u8> = if hex_mode {
        let cleaned: String = data.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if cleaned.len() % 2 != 0 {
            return Err("HEX 位数必须为偶数".into());
        }
        (0..cleaned.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|e| format!("HEX 解析失败: {}", e))?
    } else {
        let mut s = data;
        match newline.as_str() {
            "cr" => s.push('\r'),
            "lf" => s.push('\n'),
            "crlf" => s.push_str("\r\n"),
            _ => {}
        }
        encode_text(&s, enc)
    };
    core::append_checksum(
        &mut payload,
        core::ChecksumKind::from_str(&checksum),
        endian != "big",
    );
    handle.write(&payload).map(|n| n as u64).map_err(|e| format!("发送失败: {}", e))
}

#[tauri::command]
fn compute_checksum(
    data: String,
    hex_mode: bool,
    checksum: String,
    endian: String,
    state: State<'_, AppState>,
) -> String {
    let enc = state
        .sessions
        .lock()
        .unwrap()
        .values()
        .next()
        .map(|e| e.encoding.load(Ordering::Relaxed))
        .unwrap_or(0);
    let mut payload: Vec<u8> = if hex_mode {
        hex_parse(&data)
    } else {
        encode_text(&data, enc)
    };
    let kind = core::ChecksumKind::from_str(&checksum);
    core::append_checksum(&mut payload, kind, endian != "big");
    let n = kind.len_bytes();
    hex_str(&payload[payload.len().saturating_sub(n)..], 64)
}

#[tauri::command]
fn set_dtr(tab: String, on: bool, state: State<'_, AppState>) -> Result<(), String> {
    let sessions = state.sessions.lock().unwrap();
    match sessions.get(&tab) {
        Some(e) => e.handle.set_dtr(on).map_err(|e| e.to_string()),
        None => Err("该标签未连接".into()),
    }
}

#[tauri::command]
fn set_rts(tab: String, on: bool, state: State<'_, AppState>) -> Result<(), String> {
    let sessions = state.sessions.lock().unwrap();
    match sessions.get(&tab) {
        Some(e) => e.handle.set_rts(on).map_err(|e| e.to_string()),
        None => Err("该标签未连接".into()),
    }
}

#[tauri::command]
fn set_encoding(tab: String, encoding: String, state: State<'_, AppState>) {
    if let Some(e) = state.sessions.lock().unwrap().get(&tab) {
        e.encoding.store(if encoding == "gbk" { 1 } else { 0 }, Ordering::Relaxed);
    }
}

#[tauri::command]
fn set_rules(rules: Vec<RuleIn>, state: State<'_, AppState>) {
    let enc = 0u8; // 规则载荷编码:文本按 UTF-8 预编码,触发时不再转换(MVP)
    let compiled: Vec<Rule> = rules.into_iter().map(|r| Rule::from_input(r, enc)).collect();
    *state.rules.lock().unwrap() = compiled;
}

#[tauri::command]
fn set_scope(on: bool, channels: u8, protocol: String, state: State<'_, AppState>) {
    state.scope_on.store(on, Ordering::Relaxed);
    state.scope_channels.store(channels.clamp(1, 32), Ordering::Relaxed);
    *state.scope_protocol.lock().unwrap() = protocol;
    // 重建所有会话的解析器(协议/通道数变更即刻生效)
    let ch = state.scope_channels.load(Ordering::Relaxed) as usize;
    let proto = state.scope_protocol.lock().unwrap().clone();
    for e in state.sessions.lock().unwrap().values_mut() {
        *e.scope_parser.lock().unwrap() = core::make_parser(&proto, ch);
    }
}

#[derive(Serialize, Clone)]
struct StatsOut {
    connected: bool,
    rx_bytes: u64,
    tx_bytes: u64,
    reconnects: u32,
    backpressure: u64,
}

// ---------- Lua 脚本 ----------
#[tauri::command]
fn run_script(
    tab: String,
    code: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        if state.scripts.lock().unwrap().contains_key(&tab) {
            return Err("该标签已有脚本在运行".into());
        }
    }
    let session = {
        let sessions = state.sessions.lock().unwrap();
        match sessions.get(&tab) {
            Some(e) => e.handle.clone(),
            None => return Err("该标签未连接".into()),
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    state.scripts.lock().unwrap().insert(tab.clone(), Arc::clone(&stop));
    let app2 = app.clone();
    let tab2 = tab.clone();
    let tab_log = tab2.clone();
    let tab3 = tab.clone();
    let scripts_map = Arc::clone(&state.scripts);
    let stop2 = Arc::clone(&stop);
    let code2 = code.clone();
    thread::spawn(move || {
        let ctx = core::ScriptContext {
            session,
            stop,
            on_log: Box::new(move |m: &str| {
                let _ = app2.emit(
                    "rx",
                    serde_json::json!({
                        "tab": tab_log, "seq": 0,
                        "lines": [{ "ts": chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
                                    "s": format!("⚙ {m}") }],
                        "hex": "", "bytes": 0, "total_rx": 0,
                        "connected": true, "reconnects": 0,
                    }),
                );
            }),
        };
        let res = core::run_script(ctx, &code2);
        let msg = match res {
            Ok(()) => "脚本执行完成".to_string(),
            Err(e) => format!("脚本结束: {}", e.0),
        };
        let _ = app.emit(
            "rx",
            serde_json::json!({
                "tab": tab2, "seq": 0,
                "lines": [{ "ts": chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
                            "s": format!("── {msg} ──") }],
                "hex": "", "bytes": 0, "total_rx": 0,
                "connected": true, "reconnects": 0,
            }),
        );
        scripts_map.lock().unwrap().remove(&tab3);
        let _ = stop2;
    });
    Ok(())
}

#[tauri::command]
fn stop_script(tab: String, state: State<'_, AppState>) {
    if let Some(flag) = state.scripts.lock().unwrap().remove(&tab) {
        flag.store(true, Ordering::Relaxed);
    }
}

// ---------- 日志回放 ----------
#[derive(Serialize, Clone)]
struct LogInfo {
    file: String,
    size: u64,
    modified_ms: u64,
}

#[tauri::command]
fn list_logs() -> Vec<LogInfo> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("logs")))
        .unwrap_or_default();
    let mut out: Vec<LogInfo> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "bin").unwrap_or(false))
                .filter_map(|e| {
                    let md = e.metadata().ok()?;
                    Some(LogInfo {
                        file: e.file_name().to_string_lossy().to_string(),
                        size: md.len(),
                        modified_ms: md
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    out
}

/// pplog 会话日志回放:按记录 ms 时间戳忠实重放(speed=N → N 倍速,0=最快)。
fn replay_pplog(
    tab: String,
    path: PathBuf,
    speed: u32,
    stop: Arc<AtomicBool>,
    app: tauri::AppHandle,
) {
    // 预扫描:总 RX 字节数(进度显示)
    let mut total_rx: u64 = 0;
    {
        let Ok((mut r, _)) = core::pplog::PplogReader::open(&path) else { return };
        loop {
            match r.next_record() {
                Ok(Some(rec)) => {
                    if rec.dir == core::pplog::DIR_RX {
                        total_rx += rec.data.len() as u64;
                    }
                }
                _ => break,
            }
        }
    }
    let Ok((mut rdr, meta)) = core::pplog::PplogReader::open(&path) else { return };
    eprintln!(
        "[replay] pplog {} @ {}x(kind={} baud={})",
        path.display(),
        speed,
        meta.kind,
        meta.baud
    );

    let speed_f = if speed == 0 { 0.0 } else { speed as f64 };
    let start = Instant::now();
    let mut t0: Option<u64> = None;
    let mut seq: u64 = 0;
    let mut sent: u64 = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let rec = match rdr.next_record() {
            Ok(Some(r)) => r,
            Ok(None) => {
                let _ = app.emit(
                    "rx",
                    serde_json::json!({
                        "tab": tab, "seq": seq + 1, "lines": [], "hex": "",
                        "bytes": 0, "total_rx": sent, "connected": false,
                        "reconnects": 0, "replay_done": true,
                    }),
                );
                break;
            }
            Err(_) => break,
        };
        if rec.dir != core::pplog::DIR_RX {
            continue; // TX 记录暂不进 RX 展示流
        }
        let base = *t0.get_or_insert(rec.ms);
        if speed > 0 {
            let target = Duration::from_millis(((rec.ms.saturating_sub(base)) as f64 / speed_f) as u64);
            let elapsed = start.elapsed();
            if target > elapsed {
                thread::sleep(target - elapsed);
            }
        }
        sent += rec.data.len() as u64;
        let raw = String::from_utf8_lossy(&rec.data);
        let lines: Vec<LineOut> = raw
            .split('\n')
            .filter(|s| !s.trim().is_empty())
            .map(|s| LineOut { ts: String::new(), s: sanitize(s.trim_end_matches('\r')) })
            .collect();
        seq += 1;
        let _ = app.emit(
            "rx",
            serde_json::json!({
                "tab": tab, "seq": seq, "lines": lines,
                "hex": hex_str(&rec.data, 2048), "bytes": rec.data.len(),
                "total_rx": sent, "connected": true, "reconnects": 0,
                "replay_progress": format!("{}/{}", sent, total_rx),
            }),
        );
    }
    app.state::<AppState>().replays.lock().unwrap().remove(&tab);
}

#[tauri::command]
fn replay(
    tab: String,
    file: String,
    speed: u32, // 1x = 46080 B/s(460800 波特率线速);0 = 最快
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    {
        let replays = state.replays.lock().unwrap();
        if replays.contains_key(&tab) {
            return Err("该标签正在回放中,请先停止".into());
        }
    }
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("logs").join(&file)))
        .ok_or_else(|| "路径无效".to_string())?;
    let is_pplog = core::pplog::is_pplog(&path);
    let mut f = File::open(&path).map_err(|e| format!("打开日志失败: {}", e))?;
    let total = f.metadata().map(|m| m.len()).unwrap_or(0);

    let stop = Arc::new(AtomicBool::new(false));
    state.replays.lock().unwrap().insert(tab.clone(), Arc::clone(&stop));

    let app2 = app.clone();
    thread::spawn(move || {
        if is_pplog {
            replay_pplog(tab, path, speed, stop, app2);
            return;
        }
        // ---- 旧版裸字节流:按线速估算节奏(无时间戳) ----
        let mut file = f;
        let mut buf = vec![0u8; 4096];
        // 33ms 一帧;speed=1 → 46080/30 ≈ 1536 B/帧(即 1 倍 460800 线速)
        let per_tick = if speed == 0 {
            buf.len().max(1) * 8
        } else {
            ((46080u64 * speed as u64) / 30).clamp(64, 4 * 1024 * 1024) as usize
        };
        let mut seq: u64 = 0;
        let mut sent: u64 = 0;
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let mut n = 0usize;
            while n < per_tick {
                let want = per_tick - n;
                let want_n = want.min(buf.len());
                let r = file.read(&mut buf[..want_n]).unwrap_or(0);
                if r == 0 {
                    break;
                }
                n += r;
            }
            if n == 0 {
                let _ = app2.emit(
                    "rx",
                    serde_json::json!({
                        "tab": tab, "seq": seq + 1, "lines": [], "hex": "",
                        "bytes": 0, "total_rx": sent, "connected": false,
                        "reconnects": 0, "replay_done": true,
                    }),
                );
                break;
            }
            sent += n as u64;
            let raw = String::from_utf8_lossy(&buf[..n]);
            let lines: Vec<LineOut> = raw
                .split('\n')
                .filter(|s| !s.trim().is_empty())
                .map(|s| LineOut { ts: String::new(), s: sanitize(s.trim_end_matches('\r')) })
                .collect();
            seq += 1;
            let _ = app2.emit(
                "rx",
                serde_json::json!({
                    "tab": tab, "seq": seq, "lines": lines,
                    "hex": hex_str(&buf[..n], 2048), "bytes": n,
                    "total_rx": sent, "connected": true, "reconnects": 0,
                    "replay_progress": format!("{}/{}", sent, total),
                }),
            );
            thread::sleep(Duration::from_millis(33));
        }
        app.state::<AppState>().replays.lock().unwrap().remove(&tab);
    });
    Ok(format!("回放已启动:{file} @ {speed}x"))
}

#[tauri::command]
fn replay_stop(tab: String, state: State<'_, AppState>) {
    if let Some(flag) = state.replays.lock().unwrap().remove(&tab) {
        flag.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
fn stats(tab: String, state: State<'_, AppState>) -> StatsOut {
    let sessions = state.sessions.lock().unwrap();
    match sessions.get(&tab) {
        Some(e) => {
            let st = e.handle.stats();
            StatsOut {
                connected: st.connected,
                rx_bytes: st.rx_bytes,
                tx_bytes: st.tx_bytes,
                reconnects: st.reconnects,
                backpressure: st.backpressure_events,
            }
        }
        None => StatsOut { connected: false, rx_bytes: 0, tx_bytes: 0, reconnects: 0, backpressure: 0 },
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let _ = std::fs::OpenOptions::new().create(true).append(true)
            .open("portpilot-panic.log")
            .and_then(|mut f| writeln!(f, "{}", info));
    }));

    let mut cli_port = String::new();
    let mut cli_kind = String::from("serial");
    let mut cli_baud = 0u32;
    let mut cli_seconds = 0.0f64;
    let mut cli_out = String::new();
    let mut cli_decoder = String::new();
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--port" => cli_port = v,
            "--kind" => cli_kind = v,
            "--baud" => cli_baud = v.parse().unwrap_or(0),
            "--seconds" => cli_seconds = v.parse().unwrap_or(0.0),
            "--out" => cli_out = v,
            "--decoder" => cli_decoder = v,
            _ => {}
        }
    }
    // 网络传输无波特率:仅串口要求 baud>0
    let smoke = !cli_port.is_empty()
        && cli_seconds > 0.0
        && (cli_baud > 0 || cli_kind != "serial");
    let cli_dec_cfg: Option<core::DecoderConfig> = if cli_decoder.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&cli_decoder).expect("解析 --decoder 配置失败"))
    };

    // 单实例:第二次启动时聚焦已有窗口(避免两实例抢同一串口);
    // CLI 冒烟模式(--port)豁免,便于与 GUI 并存做自动化测试
    let builder = tauri::Builder::default();
    let builder = if !smoke {
        builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
    } else {
        builder
    };
    builder
        .setup(move |app| {
            let probe_file = if smoke { Some(File::create(&cli_out).expect("create probe out")) } else { None };
            *app.state::<AppState>().probe.lock().unwrap() = Probe {
                file: probe_file,
                deadline: if smoke {
                    Some(Instant::now() + Duration::from_secs_f64(cli_seconds + 5.0))
                } else {
                    None
                },
            };
            if smoke {
                let st = app.state::<AppState>();
                let res = connect("main".into(), cli_port.clone(), cli_baud, "utf8".into(), cli_kind.clone(), None, st, app.handle().clone());
                eprintln!("[smoke] connect -> {res:?}");
                if let Some(cfg) = &cli_dec_cfg {
                    let st2 = app.state::<AppState>();
                    let sessions = st2.sessions.lock().unwrap();
                    if let Some(e) = sessions.get("main") {
                        *e.decoder.lock().unwrap() = Some(core::FrameDecoder::new(cfg.clone()));
                        eprintln!("[smoke] 解码器已启用: {}", cfg.name);
                    }
                }
                // 冒烟看门狗:无论连接成败,到时退出(多会话版泵线程不再管 deadline)
                let h = app.handle().clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_secs_f64(cli_seconds + 5.0));
                    h.exit(0);
                });
            }
            Ok(())
        })
        .manage(AppState {
            sessions: Mutex::new(HashMap::new()),
            rules: Arc::new(Mutex::new(Vec::new())),
            scope_on: Arc::new(AtomicBool::new(false)),
            scope_channels: Arc::new(AtomicU8::new(4)),
            scope_protocol: Mutex::new("justfloat".into()),
            probe: Arc::new(Mutex::new(Probe { file: None, deadline: None })),
            replays: Mutex::new(HashMap::new()),
            scripts: Arc::new(Mutex::new(HashMap::new())),
        })
        .invoke_handler(tauri::generate_handler![
            list_ports, connect, disconnect, send, compute_checksum,
            set_dtr, set_rts, set_encoding, set_rules, set_scope, stats,
            list_logs, replay, replay_stop, run_script, stop_script
        ])
        .run(tauri::generate_context!())
        .expect("error while running portpilot");
}
