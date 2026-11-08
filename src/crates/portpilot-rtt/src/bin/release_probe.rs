// PortPilot · 句柄释放探针 v2:双层定位泄漏
//   测试 A:裸 open_transport → close → drop → reopen(审问 serialport 层)
//   测试 B:spawn_session → stop → drop → reopen(审问 session 线程层)
// 用法: release_probe.exe <COM口> [baud]
use portpilot_core::spawn_session;
use portpilot_core::SessionOptions;
use portpilot_core::TransportConfig;
use portpilot_core::TransportKind;
use std::time::Duration;

fn cfg(port: &str, baud: u32) -> TransportConfig {
    TransportConfig {
        kind: TransportKind::Serial,
        port: port.to_string(),
        remote: None,
        baud,
        data_bits: 8,
        stop_bits: 1,
        parity: portpilot_core::Parity::None,
        read_timeout_ms: 5,
    }
}

fn try_open(port: &str, baud: u32, tag: &str) -> bool {
    match portpilot_core::open_transport(&cfg(port, baud)) {
        Ok(mut t) => {
            eprintln!("[probe] {tag}: ✅ 打开成功(已释放)");
            t.close();
            true
        }
        Err(e) => {
            eprintln!("[probe] {tag}: ❌ 拒绝访问(句柄未释放) {e}");
            false
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port = args.get(1).cloned().unwrap_or_else(|| "COM11".into());
    let baud: u32 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(256000);

    // ---- 测试 A:裸传输 ----
    let mut t = portpilot_core::open_transport(&cfg(&port, baud)).expect("A: 初次打开失败");
    eprintln!("[A] 已打开,持有 1s ...");
    std::thread::sleep(Duration::from_millis(1000));
    t.close();
    drop(t);
    std::thread::sleep(Duration::from_millis(1500));
    let a_ok = try_open(&port, baud, "A: 裸 close+drop 后重开");
    if a_ok {
        eprintln!("[A] serialport 层释放正常,继续测试 B");
    } else {
        eprintln!("[A] ❌ serialport 层泄漏实锤,测试 B 无需进行");
        return;
    }

    // ---- 测试 B:session 层 ----
    let port2 = port.clone();
    let session = spawn_session(SessionOptions::new(Box::new(move || {
        portpilot_core::open_transport(&cfg(&port2, baud))
    })));
    eprintln!("[B] session 已起,持有 1.5s ...");
    std::thread::sleep(Duration::from_millis(1500));
    session.stop();
    drop(session);
    std::thread::sleep(Duration::from_millis(1500));
    try_open(&port, baud, "B: session stop+drop 后重开");
}
