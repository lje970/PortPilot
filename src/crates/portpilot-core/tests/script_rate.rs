// PortPilot · 集成测试:Lua 脚本 port.write 速率基准(排查 sleep/write 节拍异常)
// GUI 实测:loop 会话 + 100 行/s 目标,实测仅 ~1-8 行/s。本测试三段定位时间去向。
use portpilot_core::script::{run_script, ScriptContext};
use portpilot_core::{spawn_session, SessionOptions, TransportConfig, TransportKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn lua_write_rate_benchmark() {
    let session = spawn_session(SessionOptions::new(Box::new(|| {
        let cfg = TransportConfig {
            kind: TransportKind::Loop,
            port: "loop".into(),
            remote: None,
            baud: 0,
            data_bits: 8,
            stop_bits: 1,
            parity: portpilot_core::Parity::None,
            read_timeout_ms: 5,
        };
        portpilot_core::open_transport(&cfg)
    })));
    std::thread::sleep(Duration::from_millis(300)); // 等会话建连

    let stop = Arc::new(AtomicBool::new(false));
    let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let logs2 = Arc::clone(&logs);
    let ctx = ScriptContext {
        session: session.clone(),
        stop: Arc::clone(&stop),
        on_log: Box::new(move |s: &str| {
            logs2.lock().unwrap().push(s.to_string());
        }),
    };

    // 三段基准:纯 sleep / 纯 write / 组合,各 100 次
    let code = r#"
local t = ms()
for i = 1, 100 do sleep(10) end
log("A_pure_sleep_100: " .. (ms() - t) .. "ms (期望≈1000)")
t = ms()
for i = 1, 100 do port.write("123456\n") end
log("B_pure_write_100: " .. (ms() - t) .. "ms (期望<500)")
t = ms()
for i = 1, 200 do
  port.write("123456\n")
  sleep(10)
end
log("C_combo_200: " .. (ms() - t) .. "ms (期望≈2000)")
log("DONE")
"#;
    let t0 = Instant::now();
    let res = run_script(ctx, code);
    let dt = t0.elapsed();
    stop.store(true, Ordering::Relaxed);

    match &res {
        Ok(()) => eprintln!("[bench] 脚本完成"),
        Err(e) => eprintln!("[bench] 脚本错误: {}", e.0),
    }
    eprintln!("[bench] 三段基准总耗时 {:?}(期望 ≈3s)", dt);
    for (i, s) in logs.lock().unwrap().iter().enumerate() {
        eprintln!("[log{}] {}", i, s);
    }
    // 验收:三段合计 ≈3s(100×10ms sleep + 200×write≈0);病态时 >30s
    assert!(res.is_ok(), "脚本应无错完成");
    assert!(dt < Duration::from_secs(6), "三段基准耗时不合理: {dt:?}");
    session.stop();
}
