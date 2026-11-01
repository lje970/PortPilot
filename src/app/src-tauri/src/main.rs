// PortPilot 0.1.0 · Tauri 后端
// ==============================
// 三段式 UI 的 Rust 侧:会话管理(经 portpilot-core)、30fps 泵线程(行组装 +
// HEX 流 + 原始日志落盘 + 探针协议输出)、发送/线控命令。
// 自动化冒烟:portpilot-app.exe --port COM9 --baud 460800 --seconds 12 --out smoke.jsonl

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use portpilot_core as core;
use serde::Serialize;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager, State};

// ---------- 探针(CLI 自动化冒烟用) ----------
struct Probe {
    file: Option<File>,
    deadline: Option<Instant>,
}

// ---------- 应用状态 ----------
struct AppState {
    session: Mutex<Option<core::SessionHandle>>,
    pump_stop: Mutex<Option<Arc<AtomicBool>>>,
    log_path: Mutex<Option<PathBuf>>,
    probe: Arc<Mutex<Probe>>,
    probe_rx: Arc<AtomicU64>,
}

#[derive(Serialize, Clone)]
struct LineOut {
    ts: String, // HH:MM:SS.mmm(本地时区)
    s: String,  // 行文本(已裁控制符)
}

#[derive(Serialize, Clone)]
struct RxEvent {
    seq: u64,
    lines: Vec<LineOut>,
    hex: String, // 本批 HEX 流(≤2KB)
    bytes: usize,
    total_rx: u64,
    connected: bool,
    reconnects: u32,
}

#[derive(Serialize, Clone)]
struct PortOut {
    name: String,
    usb: String, // "1A86:7523" 或空
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else if (c as u32) < 32 { '·' } else { c })
        .collect()
}

fn wall_ts(epoch_ms_at_start: u64, arrival_ms: f64) -> String {
    let ms = epoch_ms_at_start + arrival_ms as u64;
    let dt = chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|d| d.with_timezone(&chrono::Local))
        .unwrap_or_else(chrono::Local::now);
    dt.format("%H:%M:%S%.3f").to_string()
}

fn now_epoch_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn hex_str(data: &[u8], cap: usize) -> String {
    let n = data.len().min(cap);
    let mut s = String::with_capacity(n * 3);
    for (i, b) in data[..n].iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:02x}", b));
    }
    if data.len() > cap {
        s.push_str(" …");
    }
    s
}

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
    port: String,
    baud: u32,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    {
        let guard = state.session.lock().unwrap();
        if guard.is_some() {
            return Err("已有会话在运行,请先断开".into());
        }
    }
    // 日志文件:exe 目录 logs/PortPilot_YYYYMMDD_HHMMSS.bin(原始字节流,保真可回放)
    let log_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("logs")))
        .unwrap_or_else(|| PathBuf::from("logs"));
    let _ = std::fs::create_dir_all(&log_dir);
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let log_path = log_dir.join(format!("PortPilot_{}.bin", stamp));
    let log_file = File::create(&log_path).map_err(|e| format!("创建日志失败: {}", e))?;

    let probe_rx = Arc::clone(&state.probe_rx);
    probe_rx.store(0, Ordering::Relaxed);
    let epoch_ms = now_epoch_ms();

    let port_for_factory = port.clone();
    let session = core::spawn_session(core::SessionOptions::new(Box::new(move || {
        core::open_serial(&core::TransportConfig::new(port_for_factory.clone(), baud))
    })));

    // ---- 泵线程:33ms 批量 → 行组装 + HEX + 日志 + 事件 + 探针 ----
    let stop = Arc::new(AtomicBool::new(false));
    {
        let handle = app.clone();
        let stop2 = Arc::clone(&stop);
        let session2 = session.clone();
        let probe = Arc::clone(&state.probe);
        let probe_rx2 = Arc::clone(&probe_rx);
        let app2 = app.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; 256 * 1024];
            let mut pending: Vec<u8> = Vec::new();
            let mut pending_arrival = 0.0f64;
            let mut next_frame = Instant::now();
            let mut next_sec = Instant::now();
            let pump_start = Instant::now();
            let mut seq: u64 = 0;
            let deadline = probe.lock().unwrap().deadline;
            let mut log_file = log_file;
            loop {
                let now = Instant::now();
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(d) = deadline {
                    if now >= d {
                        let _ = app2.exit(0);
                        break;
                    }
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
                probe_rx2.fetch_add(rep.bytes as u64, Ordering::Relaxed);
                let data = &buf[..rep.bytes];
                let _ = log_file.write_all(data); // 原始字节流落盘(保真,可回放)
                let arrival = rep.newest_arrival_ms.unwrap_or_else(|| session2.now_ms());

                // 组装文本行(跨批拼接 \n;二进制流无 \n 时按 2KB 强制冲刷,防止缓冲无限增长)
                pending.extend_from_slice(data);
                let mut lines: Vec<LineOut> = Vec::new();
                let mut start = 0usize;
                for i in 0..pending.len() {
                    if pending[i] == b'\n' {
                        let raw = String::from_utf8_lossy(&pending[start..i]);
                        lines.push(LineOut { ts: wall_ts(epoch_ms, pending_arrival), s: sanitize(&raw) });
                        start = i + 1;
                        pending_arrival = arrival; // 同批内沿用本批到达
                    }
                }
                pending.drain(..start);
                if pending.len() > 2048 {
                    let raw = String::from_utf8_lossy(&pending);
                    lines.push(LineOut { ts: wall_ts(epoch_ms, pending_arrival), s: sanitize(&raw) });
                    pending.clear();
                }

                let ev = RxEvent {
                    seq: seq + 1,
                    lines,
                    hex: hex_str(data, 2048),
                    bytes: rep.bytes,
                    total_rx: probe_rx2.load(Ordering::Relaxed),
                    connected: session2.stats().connected,
                    reconnects: session2.stats().reconnects,
                };
                seq = ev.seq;
                let _ = handle.emit("rx", &ev);

                // 探针行(每秒;CLI 冒烟模式)
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

            // 收尾:冲刷残留未完行(断开/退出时不丢尾巴)
            if !pending.is_empty() {
                let raw = String::from_utf8_lossy(&pending);
                let ev = RxEvent {
                    seq: seq + 1,
                    lines: vec![LineOut { ts: wall_ts(epoch_ms, pending_arrival), s: sanitize(&raw) }],
                    hex: String::new(),
                    bytes: 0,
                    total_rx: probe_rx2.load(Ordering::Relaxed),
                    connected: false,
                    reconnects: session2.stats().reconnects,
                };
                let _ = handle.emit("rx", &ev);
            }
        });
    }

    *state.session.lock().unwrap() = Some(session);
    *state.pump_stop.lock().unwrap() = Some(stop);
    *state.log_path.lock().unwrap() = Some(log_path.clone());
    Ok(format!("已连接 {port} @ {baud};日志: {}", log_path.display()))
}

#[tauri::command]
fn disconnect(state: State<'_, AppState>) -> Result<String, String> {
    if let Some(stop) = state.pump_stop.lock().unwrap().take() {
        stop.store(true, Ordering::Relaxed);
    }
    if let Some(s) = state.session.lock().unwrap().take() {
        s.stop();
        let st = s.stats();
        Ok(format!("已断开;本次会话 RX={}B TX={}B 重连={}", st.rx_bytes, st.tx_bytes, st.reconnects))
    } else {
        Err("当前没有会话".into())
    }
}

#[tauri::command]
fn send(data: String, hex_mode: bool, newline: String, state: State<'_, AppState>) -> Result<u64, String> {
    let bytes: Vec<u8> = if hex_mode {
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
        data.as_bytes().to_vec()
    };
    let mut payload = bytes;
    match newline.as_str() {
        "cr" => payload.push(b'\r'),
        "lf" => payload.push(b'\n'),
        "crlf" => payload.extend_from_slice(b"\r\n"),
        _ => {}
    }
    let guard = state.session.lock().unwrap();
    match guard.as_ref() {
        Some(s) => s.write(&payload).map(|n| n as u64).map_err(|e| format!("发送失败: {}", e)),
        None => Err("未连接".into()),
    }
}

#[tauri::command]
fn set_dtr(on: bool, state: State<'_, AppState>) -> Result<(), String> {
    let guard = state.session.lock().unwrap();
    match guard.as_ref() {
        Some(s) => s.set_dtr(on).map_err(|e| e.to_string()),
        None => Err("未连接".into()),
    }
}

#[tauri::command]
fn set_rts(on: bool, state: State<'_, AppState>) -> Result<(), String> {
    let guard = state.session.lock().unwrap();
    match guard.as_ref() {
        Some(s) => s.set_rts(on).map_err(|e| e.to_string()),
        None => Err("未连接".into()),
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
fn stats(state: State<'_, AppState>) -> StatsOut {
    let guard = state.session.lock().unwrap();
    match guard.as_ref() {
        Some(s) => {
            let st = s.stats();
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

    // CLI 冒烟参数
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
            let probe_file = if smoke {
                Some(File::create(&cli_out).expect("create probe out"))
            } else {
                None
            };
            *app.state::<AppState>().probe.lock().unwrap() = Probe {
                file: probe_file,
                deadline: if smoke {
                    Some(Instant::now() + Duration::from_secs_f64(cli_seconds + 5.0))
                } else {
                    None
                },
            };

            // 冒烟:自动连接
            if smoke {
                let st = app.state::<AppState>();
                let res = connect(
                    cli_port.clone(),
                    cli_baud,
                    st,
                    app.handle().clone(),
                );
                eprintln!("[smoke] connect({cli_port}, {cli_baud}) -> {res:?}");
            }
            Ok(())
        })
        .manage(AppState {
            session: Mutex::new(None),
            pump_stop: Mutex::new(None),
            log_path: Mutex::new(None),
            probe: Arc::new(Mutex::new(Probe { file: None, deadline: None })),
            probe_rx: Arc::new(AtomicU64::new(0)),
        })
        .invoke_handler(tauri::generate_handler![
            list_ports, connect, disconnect, send, set_dtr, set_rts, stats
        ])
        .run(tauri::generate_context!())
        .expect("error while running portpilot");
}
