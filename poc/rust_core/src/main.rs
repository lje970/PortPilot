// POC-A:Rust(serialport crate)+ 原生线程 最小串口收流核心(console)。
// 这是 Tauri 方案的"后端核心"同款代码:I/O 线程阻塞读 → mpsc → 33ms 节流消费(渲染)。
// GUI(Tauri)脚手架见 ../tauri_app(本环境无 MSVC,GUI 未编译;核心层实测即此工程)。
// 遵循 poc/probe_protocol.md v1。

use std::env;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

struct Args {
    port: String,
    baud: u32,
    seconds: f64,
    out: String,
}

fn parse_args() -> Args {
    let mut a = Args { port: "COM1".into(), baud: 921600, seconds: 30.0, out: "probe_results.jsonl".into() };
    let mut it = env::args().skip(1);
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

fn open_port(name: &str, baud: u32) -> Result<Box<dyn serialport::SerialPort>, String> {
    serialport::new(name, baud)
        .timeout(Duration::from_millis(5))
        .open()
        .map_err(|e| e.to_string())
}

fn main() {
    let args = parse_args();
    let seconds = args.seconds;
    let out = args.out.clone();
    let port_name = args.port.clone();
    let baud = args.baud;

    let reconnects = Arc::new(AtomicU32::new(0));
    let (tx, rx) = mpsc::channel::<(Instant, Vec<u8>)>();

    // ---- I/O 线程:阻塞读,断线自动重连 ----
    {
        let reconnects = reconnects.clone();
        thread::spawn(move || {
            let mut maybe = match open_port(&port_name, baud) {
                Ok(p) => {
                    eprintln!("[rust] opened {} @ {}", port_name, baud);
                    Some(p)
                }
                Err(e) => {
                    eprintln!("[rust] open {} failed: {}", port_name, e);
                    std::process::exit(2);
                }
            };
            let mut buf = [0u8; 4096];
            let end = Instant::now() + Duration::from_secs_f64(seconds + 5.0);
            while Instant::now() < end {
                let port = match maybe.as_mut() {
                    Some(p) => p,
                    None => {
                        thread::sleep(Duration::from_millis(500));
                        match open_port(&port_name, baud) {
                            Ok(p) => {
                                eprintln!("[rust] reconnected {}", port_name);
                                maybe = Some(p);
                                continue;
                            }
                            Err(e) => {
                                eprintln!("[rust] reopen failed: {}", e);
                                continue;
                            }
                        }
                    }
                };
                match port.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        let _ = tx.send((Instant::now(), buf[..n].to_vec()));
                    }
                    Err(e) => {
                        let is_timeout = e.kind() == std::io::ErrorKind::TimedOut
                            || e.to_string().to_lowercase().contains("timed out");
                        if is_timeout {
                            continue;
                        }
                        eprintln!("[rust] read error: {} — reconnect in 500ms", e);
                        reconnects.fetch_add(1, Ordering::Relaxed);
                        maybe = None;
                    }
                }
            }
        });
    }

    // ---- 消费/渲染循环:33ms 一帧,每秒探针行 ----
    let mut file = File::create(&out).expect("create probe out");
    let mut rx_bytes: usize = 0;
    let mut renders: u32 = 0;
    let mut lat_samples: Vec<f64> = Vec::new();
    let mut iar_samples: Vec<f64> = Vec::new();
    let mut last_arrival: Option<Instant> = None;
    let start = Instant::now();
    let mut next_frame = start;
    let mut next_sec = start;
    let mut sec: u32 = 0;

    loop {
        let now = Instant::now();
        if now >= start + Duration::from_secs_f64(seconds + 5.0) {
            break;
        }
        // 帧节流
        if now < next_frame {
            thread::sleep(Duration::from_micros(500));
            continue;
        }
        next_frame += Duration::from_millis(33);
        let mut got = false;
        let mut newest_arr: Option<Instant> = None;
        while let Ok((arr, data)) = rx.try_recv() {
            if let Some(last) = last_arrival {
                iar_samples.push(arr.duration_since(last).as_secs_f64() * 1000.0);
            }
            last_arrival = Some(arr);
            newest_arr = Some(arr);
            rx_bytes += data.len();
            got = true;
        }
        if got {
            renders += 1;
            if let Some(arr) = newest_arr {
                lat_samples.push(now.duration_since(arr).as_secs_f64() * 1000.0);
            }
            print!("\r[rust] {} B  {} fps  ", rx_bytes, renders);
            let _ = std::io::stdout().flush();
        }
        // 每秒探针行
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
                sec, rx_bytes, renders as f64, p95(&mut lat_samples), p95(&mut iar_samples),
                reconnects.load(Ordering::Relaxed)
            );
            let _ = file.write_all(line.as_bytes());
            renders = 0;
        }
    }
    println!("\n[rust] done: {} B in {:.1}s", rx_bytes, seconds);
}
