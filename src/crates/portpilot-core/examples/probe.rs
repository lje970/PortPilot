//! 真机验证示例:与 poc/tauri_app 同款探针协议(v1),跑 portpilot-core。
//!
//! 前置:ESP32-S3 灌流固件(见 bench/mcu_firmware/)+ CH340 模块接线。
//! 运行:cargo run -p portpilot-core --example probe -- --port COM9 --baud 460800 --seconds 30 --out probe.jsonl
//! 验收:对照 bench/loopback_test.py --rx-only 的结果,rx_bytes 应一致且丢包为 0。

use portpilot_core::{p95, spawn_session, SessionOptions, TransportConfig};
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

fn main() {
    let mut port = String::from("COM9");
    let mut baud = 460800u32;
    let mut seconds = 30.0f64;
    let mut out = String::from("probe_results.jsonl");
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--port" => port = v,
            "--baud" => baud = v.parse().unwrap_or(baud),
            "--seconds" => seconds = v.parse().unwrap_or(seconds),
            "--out" => out = v,
            _ => {}
        }
    }

    let port_for_factory = port.clone();
    let session = spawn_session(SessionOptions::new(Box::new(move || {
        portpilot_core::open_serial(&TransportConfig::new(port_for_factory.clone(), baud))
    })));
    eprintln!("[probe] 会话已启动 {port} @ {baud},运行 {seconds}s + 5s 排水");

    let mut file = File::create(&out).expect("create probe out");
    let start = Instant::now();
    let mut next_sec = start;
    let mut next_frame = start;
    let mut sec = 0u32;
    let mut rx_bytes_total = 0u64;
    let mut renders = 0u32;
    let mut lat_samples: Vec<f64> = Vec::new();
    let mut buf = vec![0u8; 256 * 1024];

    loop {
        let now = Instant::now();
        if now >= start + Duration::from_secs_f64(seconds + 5.0) {
            break;
        }
        if now < next_frame {
            std::thread::sleep(Duration::from_micros(500));
            continue;
        }
        next_frame += Duration::from_millis(33);
        let rep = session.drain(&mut buf);
        if rep.bytes > 0 {
            rx_bytes_total += rep.bytes as u64;
            renders += 1;
            if let Some(arr) = rep.newest_arrival_ms {
                lat_samples.push(session.now_ms() - arr);
            }
        }
        if now >= next_sec {
            sec += 1;
            next_sec += Duration::from_secs(1);
            let st = session.stats();
            let line = format!(
                "{{\"t\": {}, \"rx_bytes\": {}, \"ui_fps\": {:.1}, \"latency_p95_ms\": {:.2}, \"inter_arrival_p95_ms\": 0.0, \"reconnects\": {}, \"backpressure\": {}, \"connected\": {}}}\n",
                sec, st.rx_bytes, renders as f64, p95(&mut lat_samples), st.reconnects, st.backpressure_events, st.connected
            );
            file.write_all(line.as_bytes()).unwrap();
            file.flush().unwrap();
            renders = 0;
        }
    }
    let st = session.stats();
    println!(
        "[probe] done: {} B in {:.1}s | reconnects={} backpressure={} ring_peak={}B",
        rx_bytes_total, seconds, st.reconnects, st.backpressure_events, st.ring_peak
    );
}
