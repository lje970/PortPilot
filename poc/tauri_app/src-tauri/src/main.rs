// POC-A GUI 形态:Tauri 2 壳 + 与 ../rust_core 同款的串口核心
// (I/O 线程阻塞读 + 自动重连 → mpsc → 33ms 泵线程批量 emit → 前端渲染后回报延迟)。
// 遵循 poc/probe_protocol.md v1:
//   --port COM1 --baud 921600 --seconds 30 --out probe_results.jsonl
// 口径说明:latency = Rust emit 事件 → 前端渲染并 invoke('rendered') 回到 Rust 的往返,
// 与 Electron(主进程 sentAt → 渲染回报)口径一致。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};

struct Args {
    port: String,
    baud: u32,
    seconds: f64,
    out: String,
}

fn parse_args() -> Args {
    let mut a = Args { port: "COM1".into(), baud: 921600, seconds: 30.0, out: "probe_results.jsonl".into() };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--port" => a.port = v,
            "--baud" => a.baud = v.parse().unwrap_or(a.baud),
            "--seconds" => a.seconds = v.parse().unwrap_or(a.seconds),
            "--out" => a.out = v,
            _ => {}
        }
    }
    a
}

fn ms_since(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[derive(Clone, Serialize)]
struct BatchEvent {
    seq: u64,
    bytes: usize,
    preview: String,
    newest_arrival_ms: f64,
    emit_ms: f64,
}

struct AppState {
    start: Instant,
    lat_samples: Arc<Mutex<Vec<f64>>>,
    js_log_path: Arc<Mutex<String>>,
}

#[tauri::command]
fn rendered(seq: u64, emit_ms: f64, state: State<AppState>) {
    let lat = ms_since(state.start) - emit_ms;
    if lat >= 0.0 {
        state.lat_samples.lock().unwrap().push(lat);
    }
    let _ = seq;
}

#[tauri::command]
fn js_error(msg: String, state: State<AppState>) {
    eprintln!("[tauri][js-error] {}", msg);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
        .open(format!("{}.jserr", state.js_log_path.lock().unwrap().clone()))
    {
        let _ = writeln!(f, "{}", msg);
    }
    let _ = state;
}

#[tauri::command]
fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| match &p.port_type {
            serialport::SerialPortType::UsbPort(info) => {
                format!("{} (USB {:04x}:{:04x})", p.port_name, info.vid, info.pid)
            }
            _ => p.port_name.clone(),
        })
        .collect()
}

fn main() {
    // panic 落盘:GUI 子系统下 stderr 不可见,panic 会静默吞线程
    std::panic::set_hook(Box::new(|info| {
        let _ = std::fs::OpenOptions::new().create(true).append(true)
            .open(r"D:\serialport\poc\tauri_app\panic.log")
            .and_then(|mut f| writeln!(f, "{}", info));
    }));
    let args = parse_args();
    let seconds = args.seconds;
    let port = args.port.clone();
    let baud = args.baud;

    tauri::Builder::default()
        .setup(move |app| {
            let start = Instant::now();
            let reconnects = Arc::new(AtomicU32::new(0));
            let lat_samples = Arc::new(Mutex::new(Vec::new()));
            let (tx, rx) = mpsc::channel::<(f64, Vec<u8>)>(); // (arrival_ms, data)

            // ---- I/O 线程:阻塞读,断线自动重连(退避 500ms) ----
            {
                let reconnects = reconnects.clone();
                let stop = Arc::new(AtomicBool::new(false));
                let stop2 = stop.clone();
                let port2 = port.clone();
                thread::spawn(move || {
                    let end = Instant::now() + Duration::from_secs_f64(seconds + 5.0);
                    while Instant::now() < end && !stop2.load(Ordering::Relaxed) {
                        match serialport::new(&port2, baud).timeout(Duration::from_millis(5)).open() {
                            Ok(mut p) => {
                                eprintln!("[tauri] opened {} @ {}", port2, baud);
                                let mut buf = [0u8; 4096];
                                let mut last_arr: Option<Instant> = None;
                                while Instant::now() < end && !stop2.load(Ordering::Relaxed) {
                                    match p.read(&mut buf) {
                                        Ok(0) => {}
                                        Ok(n) => {
                                            last_arr = Some(Instant::now());
                                            let _ = tx.send((ms_since(start), buf[..n].to_vec()));
                                        }
                                        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                                        Err(e) => {
                                            eprintln!("[tauri] read error: {} → reconnect in 500ms", e);
                                            reconnects.fetch_add(1, Ordering::Relaxed);
                                            break; // 回到外层重开
                                        }
                                    }
                                }
                                let _ = last_arr;
                            }
                            Err(e) => {
                                eprintln!("[tauri] open {} failed: {} → retry in 500ms", port2, e);
                                reconnects.fetch_add(1, Ordering::Relaxed);
                                thread::sleep(Duration::from_millis(500));
                            }
                        }
                    }
                });
            }

            // ---- 泵线程:33ms 批量 emit + 每秒探针行 + 到时退出 ----
            {
                let handle = app.handle().clone();
                let reconnects = reconnects.clone();
                let lat = lat_samples.clone();
                let out = args.out.clone();
                thread::spawn(move || {
                    let mut file = BufWriter::new(File::create(&out).expect("create probe out"));
                    let deadline = Instant::now() + Duration::from_secs_f64(seconds + 5.0);
                    let mut next_frame = Instant::now();
                    let mut next_sec = Instant::now();
                    let mut seq: u64 = 0;
                    let mut rx_bytes: usize = 0;
                    let mut renders: u32 = 0;
                    let mut sec: u32 = 0;
                    let mut last_arrival_ms: Option<f64> = None;
                    let mut iar: Vec<f64> = Vec::new();

                    while Instant::now() < deadline {
                        let now = Instant::now();
                        if now < next_frame {
                            thread::sleep(Duration::from_micros(500));
                            continue;
                        }
                        next_frame += Duration::from_millis(33);
                        let mut batch = 0usize;
                        let mut newest_arr = 0.0f64;
                        let mut first_chunk: Option<Vec<u8>> = None;
                        while let Ok((arr, data)) = rx.try_recv() {
                            if let Some(last) = last_arrival_ms {
                                if arr > last {
                                    iar.push(arr - last);
                                }
                            }
                            last_arrival_ms = Some(arr);
                            if first_chunk.is_none() {
                                first_chunk = Some(data.clone());
                            }
                            batch += data.len();
                            newest_arr = arr;
                        }
                        if batch > 0 {
                            seq += 1;
                            renders += 1;
                            rx_bytes += batch;
                            if std::env::var("TAURI_NO_EMIT").is_err() {
                                let ev = BatchEvent {
                                seq,
                                bytes: batch,
                                preview: first_chunk
                                    .map(|d| d[..d.len().min(32)].iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" "))
                                    .unwrap_or_default(),
                                newest_arrival_ms: newest_arr,
                                    emit_ms: ms_since(start),
                                };
                                let _ = handle.emit("batch", &ev);
                            }
                        }
                        if now >= next_sec {
                            sec += 1;
                            next_sec += Duration::from_secs(1);
                            let p95 = |v: &mut Vec<f64>| -> f64 {
                                if v.is_empty() { return 0.0; }
                                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                                v[((v.len() as f64 * 0.95) as usize).saturating_sub(1)]
                            };
                            let line = format!(
                                "{{\"t\": {}, \"rx_bytes\": {}, \"ui_fps\": {:.1}, \"latency_p95_ms\": {:.2}, \"inter_arrival_p95_ms\": {:.2}, \"reconnects\": {}}}\n",
                                sec, rx_bytes, renders as f64,
                                p95(&mut lat.lock().unwrap().clone()),
                                p95(&mut iar),
                                reconnects.load(Ordering::Relaxed)
                            );
                            let _ = file.write_all(line.as_bytes());
                            let _ = file.flush();
                            renders = 0;
                            iar.clear();
                        }
                    }
                    let _ = file.flush();
                    handle.exit(0);
                });
            }

            app.manage(AppState { start, lat_samples, js_log_path: Arc::new(Mutex::new(args.out.clone())) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![rendered, list_ports, js_error])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
