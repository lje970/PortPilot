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
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
struct ScopeEvent { tab: String, ch: Vec<f32> }

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
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    {
        let sessions = state.sessions.lock().unwrap();
        if sessions.contains_key(&tab) {
            return Err("该标签已有会话,请先断开".into());
        }
    }
    let log_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("logs")))
        .unwrap_or_else(|| PathBuf::from("logs"));
    let _ = std::fs::create_dir_all(&log_dir);
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S%.3f");
    let log_path = log_dir.join(format!("PortPilot_{}_{}.bin", tab, stamp));
    let log_file = File::create(&log_path).map_err(|e| format!("创建日志失败: {}", e))?;

    let enc_val = if encoding == "gbk" { 1u8 } else { 0u8 };
    let encoding_a8 = Arc::new(AtomicU8::new(enc_val));
    let scope_parser: Arc<Mutex<Box<dyn core::ScopeParser>>> = Arc::new(Mutex::new(
        core::make_parser(
            &state.scope_protocol.lock().unwrap().clone(),
            state.scope_channels.load(Ordering::Relaxed) as usize,
        ),
    ));

    let port_for_factory = port.clone();
    let session = core::spawn_session(core::SessionOptions::new(Box::new(move || {
        core::open_serial(&core::TransportConfig::new(port_for_factory.clone(), baud))
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
        let th_enc = Arc::clone(&encoding_a8);
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
                if rep.bytes == 0 {
                    continue;
                }
                let data = &buf[..rep.bytes];
                let _ = log_file.write_all(data); // 原始字节流落盘
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

                // 示波器抽头
                if scope_on.load(Ordering::Relaxed) {
                    let mut frames = scope_parser2.lock().unwrap().feed(data);
                    if let Some(last) = frames.pop() {
                        let _ = handle.emit(
                            "scope",
                            ScopeEvent { tab: tab2.clone(), ch: last },
                        );
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

                // 探针行(每秒;CLI 冒烟)
                if now >= next_sec {
                    next_sec += Duration::from_secs(1);
                    let mut p = probe.lock().unwrap();
                    if let Some(f) = p.file.as_mut() {
                        let st = session2.stats();
                        let _ = writeln!(
                            f,
                            "{{\"t\": {}, \"rx_bytes\": {}, \"tx_bytes\": {}, \"connected\": {}, \"reconnects\": {}, \"backpressure\": {}}}",
                            pump_start.elapsed().as_secs(),
                            st.rx_bytes, st.tx_bytes, st.connected, st.reconnects, st.backpressure_events
                        );
                    }
                }
            }
        });
    }

    let entry = SessionEntry {
        handle: session,
        stop,
        log_path: log_path.clone(),
        encoding: encoding_a8,
        scope_parser,
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
            e.stop.store(true, Ordering::Relaxed);
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
    let mut cli_baud = 0u32;
    let mut cli_seconds = 0.0f64;
    let mut cli_out = String::new();
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--port" => cli_port = v,
            "--baud" => cli_baud = v.parse().unwrap_or(0),
            "--seconds" => cli_seconds = v.parse().unwrap_or(0.0),
            "--out" => cli_out = v,
            _ => {}
        }
    }
    let smoke = !cli_port.is_empty() && cli_baud > 0 && cli_seconds > 0.0;

    tauri::Builder::default()
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
                let res = connect("main".into(), cli_port.clone(), cli_baud, "utf8".into(), st, app.handle().clone());
                eprintln!("[smoke] connect -> {res:?}");
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
        })
        .invoke_handler(tauri::generate_handler![
            list_ports, connect, disconnect, send, compute_checksum,
            set_dtr, set_rts, set_encoding, set_rules, set_scope, stats
        ])
        .run(tauri::generate_context!())
        .expect("error while running portpilot");
}
