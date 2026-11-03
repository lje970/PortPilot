//! 会话:传输工厂 + I/O 线程(自动重连)+ 免分配环形缓冲 + 统计快照 + 发送/线控。
//!
//! UI 以固定帧率调用 [`SessionHandle::drain`],永不直接触碰串口;
//! I/O 线程内唯一堆分配发生在建连时刻,热路径(读→入环)零分配。
//! 传输实例放在共享槽(`Arc<Mutex<Option<...>>>`)中:读线程每次读短暂加锁,
//! 写命令/线控经同一槽发送(读 5ms 一轮、写低频,锁竞争可忽略)。

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::ring::SpscRing;
use crate::transport::Transport;

/// 传输工厂:每次(重)连接时调用一次,返回新传输实例。
pub type TransportFactory = Box<dyn FnMut() -> io::Result<Box<dyn Transport>> + Send>;

pub struct SessionOptions {
    pub factory: TransportFactory,
    /// 断线重连间隔(默认 500ms)
    pub reconnect_delay_ms: u64,
    /// 字节环形缓冲容量(2 的幂,默认 256KB —— 10Mbps 下约 25ms 余量)
    pub ring_capacity: usize,
}

impl SessionOptions {
    pub fn new(factory: TransportFactory) -> Self {
        Self { factory, reconnect_delay_ms: 500, ring_capacity: 256 * 1024 }
    }
}

/// 每秒/每帧可读取的统计快照。
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub reconnects: u32,
    pub connected: bool,
    /// 环形缓冲满导致的写侧自旋等待次数(>0 说明消费端跟不上)
    pub backpressure_events: u64,
    /// 到达时间戳环溢出(丢失的到达样本数,仅影响延迟统计精度)
    pub arrival_dropped: u64,
    pub ring_used: usize,
    pub ring_peak: usize,
}

#[derive(Clone)]
pub struct SessionHandle {
    stop: Arc<AtomicBool>,
    ring: Arc<SpscRing>,
    arrivals: Arc<SpscRing>, // 每读块 8B LE u64(相对会话起始的毫秒)
    rx_bytes: Arc<AtomicU64>,
    tx_bytes: Arc<AtomicU64>,
    reconnects: Arc<AtomicU32>,
    connected: Arc<AtomicBool>,
    backpressure: Arc<AtomicU64>,
    arrival_dropped: Arc<AtomicU64>,
    start: Arc<Mutex<Instant>>,
    /// 共享传输槽:读线程持有/更换实例;写命令经此发送(锁竞争可忽略)
    slot: Arc<Mutex<Option<Box<dyn Transport>>>>,
    /// 命名抽头:泵线程每批数据同步投递给注册者(Lua 脚本/MCP 读通道复用)
    taps: Arc<Mutex<Vec<(String, Box<dyn FnMut(&[u8]) + Send>)>>>,
}

pub struct DrainReport {
    pub bytes: usize,
    /// 本批最新到达时间(相对会话起始毫秒);UI 侧用它计算 上屏延迟 = now - 该值
    pub newest_arrival_ms: Option<f64>,
}

impl SessionHandle {
    /// UI 帧回调:取出环形缓冲中积压的全部字节。
    pub fn drain(&self, out: &mut [u8]) -> DrainReport {
        let bytes = self.ring.pop_slice(out);
        // 取走对应的到达时间戳(每块 8B;取到最后一个即"最新到达")
        let mut newest = None;
        let mut tmp = [0u8; 8];
        while self.arrivals.pop_slice(&mut tmp) == 8 {
            let ms = u64::from_le_bytes(tmp) as f64;
            newest = Some(ms);
        }
        DrainReport { bytes, newest_arrival_ms: newest }
    }

    pub fn stats(&self) -> StatsSnapshot {
        StatsSnapshot {
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            connected: self.connected.load(Ordering::Relaxed),
            backpressure_events: self.backpressure.load(Ordering::Relaxed),
            arrival_dropped: self.arrival_dropped.load(Ordering::Relaxed),
            ring_used: self.ring.used(),
            ring_peak: self.ring.peak_used(),
        }
    }

    /// 当前时刻相对会话起始的毫秒数(计算上屏延迟用)。
    pub fn now_ms(&self) -> f64 {
        self.start.lock().unwrap().elapsed().as_secs_f64() * 1000.0
    }

    /// 发送数据(经共享传输槽;未连接返回 NotConnected)。
    pub fn write(&self, data: &[u8]) -> io::Result<usize> {
        let n = {
            let mut guard = self.slot.lock().unwrap();
            match guard.as_mut() {
                Some(t) => t.write(data)?,
                None => return Err(io::Error::new(io::ErrorKind::NotConnected, "会话未连接")),
            }
        };
        self.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }

    /// DTR 线控(复位板卡)。
    pub fn set_dtr(&self, on: bool) -> io::Result<()> {
        let mut guard = self.slot.lock().unwrap();
        match guard.as_mut() {
            Some(t) => {
                t.set_dtr(on);
                Ok(())
            }
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "会话未连接")),
        }
    }

    /// RTS 线控。
    pub fn set_rts(&self, on: bool) -> io::Result<()> {
        let mut guard = self.slot.lock().unwrap();
        match guard.as_mut() {
            Some(t) => {
                t.set_rts(on);
                Ok(())
            }
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "会话未连接")),
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 注册命名抽头:泵线程每批数据同步投递(Lua/MCP 的读通道)。
    pub fn add_tap(&self, name: &str, cb: Box<dyn FnMut(&[u8]) + Send>) {
        self.taps.lock().unwrap().push((name.to_string(), cb));
    }

    pub fn remove_tap(&self, name: &str) {
        self.taps.lock().unwrap().retain(|(n, _)| n != name);
    }
}

/// 启动会话 I/O 线程。返回句柄后立即生效;`stop()` 令线程在 ≤ 读超时 + 重连间隔内退出。
pub fn spawn_session(opts: SessionOptions) -> SessionHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let ring = Arc::new(SpscRing::with_capacity(opts.ring_capacity));
    let arrivals = Arc::new(SpscRing::with_capacity(4096 * 8)); // 4096 个 u64
    let rx_bytes = Arc::new(AtomicU64::new(0));
    let tx_bytes = Arc::new(AtomicU64::new(0));
    let reconnects = Arc::new(AtomicU32::new(0));
    let connected = Arc::new(AtomicBool::new(false));
    let backpressure = Arc::new(AtomicU64::new(0));
    let arrival_dropped = Arc::new(AtomicU64::new(0));
    let start = Arc::new(Mutex::new(Instant::now()));
    let slot: Arc<Mutex<Option<Box<dyn Transport>>>> = Arc::new(Mutex::new(None));

    let mut factory = opts.factory;
    let delay = Duration::from_millis(opts.reconnect_delay_ms.max(50));
    let (th_stop, th_ring, th_arr) = (Arc::clone(&stop), Arc::clone(&ring), Arc::clone(&arrivals));
    let (th_rx, th_rec, th_conn) =
        (Arc::clone(&rx_bytes), Arc::clone(&reconnects), Arc::clone(&connected));
    let (th_bp, th_adr, th_start, th_slot) =
        (Arc::clone(&backpressure), Arc::clone(&arrival_dropped), Arc::clone(&start), Arc::clone(&slot));
    let taps_out: Arc<Mutex<Vec<(String, Box<dyn FnMut(&[u8]) + Send>)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let th_taps = Arc::clone(&taps_out);

    thread::spawn(move || {
        let mut buf = vec![0u8; 8192]; // 唯一一次堆分配(线程栈上大缓冲会爆栈)
        let mut read_errs = 0u32;      // 瞬时读错误连续计数(≥3 判定断线)
        let t0 = *th_start.lock().unwrap();
        let ms_since = |t0: Instant| t0.elapsed().as_secs_f64() * 1000.0;

        'outer: while !th_stop.load(Ordering::Relaxed) {
            // ---- 建连:实例放入共享槽(读/写两侧共用) ----
            match factory() {
                Ok(t) => {
                    *th_slot.lock().unwrap() = Some(t);
                    th_conn.store(true, Ordering::Relaxed);
                }
                Err(_) => {
                    th_rec.fetch_add(1, Ordering::Relaxed);
                    th_conn.store(false, Ordering::Relaxed);
                    thread::sleep(delay);
                    continue 'outer;
                }
            }

            // ---- 读循环 ----
            loop {
                if th_stop.load(Ordering::Relaxed) {
                    if let Some(mut t) = th_slot.lock().unwrap().take() {
                        t.close();
                    }
                    th_conn.store(false, Ordering::Relaxed);
                    break 'outer;
                }
                let res = {
                    let mut guard = th_slot.lock().unwrap();
                    match guard.as_mut() {
                        Some(t) => t.read(&mut buf),
                        None => Err(io::Error::new(io::ErrorKind::NotConnected, "传输槽为空")),
                    }
                };
                match res {
                    Ok(0) => {
                        read_errs = 0; // 正常超时,清空瞬时错误计数
                    }
                    Ok(n) => {
                        read_errs = 0;
                        // 入环:满则自旋等待消费端(背压),不丢数据
                        let mut pushed = 0usize;
                        while pushed < n {
                            let k = th_ring.push_slice(&buf[pushed..n]);
                            pushed += k;
                            if pushed < n {
                                th_bp.fetch_add(1, Ordering::Relaxed);
                                thread::sleep(Duration::from_micros(200));
                                if th_stop.load(Ordering::Relaxed) {
                                    if let Some(mut t) = th_slot.lock().unwrap().take() {
                                        t.close();
                                    }
                                    th_conn.store(false, Ordering::Relaxed);
                                    break 'outer;
                                }
                            }
                        }
                        th_rx.fetch_add(n as u64, Ordering::Relaxed);
                        // 到达时间戳(毫秒整数,仅统计用途,环满则丢弃样本并计数)
                        let ms_u64 = ms_since(t0) as u64;
                        if th_arr.push_slice(&ms_u64.to_le_bytes()) != 8 {
                            th_adr.fetch_add(1, Ordering::Relaxed);
                        }
                        // 命名抽头投递(Lua/MCP 读通道)
                        {
                            let mut taps = th_taps.lock().unwrap();
                            for (_, cb) in taps.iter_mut() {
                                cb(&buf[..n]);
                            }
                        }
                    }
                    Err(_) => {
                        // 瞬时错误容忍:连续 3 次读错误才判定断线
                        //(Windows 串口驱动偶发 IO 错误,不应立即重建会话)
                        read_errs += 1;
                        if read_errs < 3 {
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        read_errs = 0;
                        // 断线:取走并关闭,进入重连
                        if let Some(mut t) = th_slot.lock().unwrap().take() {
                            t.close();
                        }
                        th_conn.store(false, Ordering::Relaxed);
                        th_rec.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(delay);
                        continue 'outer;
                    }
                }
            }
        }
        if let Some(mut t) = th_slot.lock().unwrap().take() {
            t.close();
        }
        th_conn.store(false, Ordering::Relaxed);
    });

    SessionHandle { stop, ring, arrivals, rx_bytes, tx_bytes, reconnects, connected, backpressure, arrival_dropped, start, slot, taps: taps_out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::sync::atomic::AtomicUsize;

    /// 脚本化假传输:按预置块回放数据;可指定第 N 次 read 后断线;记录写入。
    struct ScriptedTransport {
        chunks: Vec<Vec<u8>>,
        idx: usize,
        err_after_read: Option<usize>,
        reads: usize,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl ScriptedTransport {
        fn new(chunks: Vec<Vec<u8>>, err_after_read: Option<usize>, writes: Arc<Mutex<Vec<Vec<u8>>>>) -> Self {
            Self { chunks, idx: 0, err_after_read, reads: 0, writes }
        }
    }

    impl Transport for ScriptedTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if let Some(n) = self.err_after_read {
                if self.reads > n {
                    return Err(io::Error::new(ErrorKind::UnexpectedEof, "脚本断线"));
                }
            }
            if self.idx < self.chunks.len() {
                let c = &self.chunks[self.idx];
                self.idx += 1;
                let n = c.len().min(buf.len());
                buf[..n].copy_from_slice(&c[..n]);
                Ok(n)
            } else {
                Ok(0) // 空转(等价读超时)
            }
        }
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.writes.lock().unwrap().push(data.to_vec());
            Ok(data.len())
        }
        fn close(&mut self) {}
    }

    fn wait_until<F: Fn() -> bool>(timeout_ms: u64, cond: F) -> bool {
        let t0 = Instant::now();
        while (t0.elapsed().as_millis() as u64) < timeout_ms {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn happy_path_receives_all_bytes() {
        let opts = SessionOptions::new(Box::new(|| {
            Ok(Box::new(ScriptedTransport::new(
                vec![b"hello ".to_vec(), b"world".to_vec()],
                None,
                Arc::new(Mutex::new(Vec::new())),
            )))
        }));
        let s = spawn_session(opts);
        assert!(wait_until(2000, || s.stats().rx_bytes >= 11));
        let st = s.stats();
        assert_eq!(st.rx_bytes, 11);
        assert_eq!(st.reconnects, 0);
        assert!(st.connected);
        let mut out = [0u8; 32];
        let rep = s.drain(&mut out);
        assert_eq!(rep.bytes, 11);
        assert_eq!(&out[..11], b"hello world");
        assert!(rep.newest_arrival_ms.is_some());
        s.stop();
    }

    #[test]
    fn reconnect_after_error_preserves_order() {
        let call = Arc::new(AtomicU32::new(0));
        let call2 = Arc::clone(&call);
        let opts = SessionOptions::new(Box::new(move || {
            let n = call2.fetch_add(1, Ordering::Relaxed);
            Ok(match n {
                0 => Box::new(ScriptedTransport::new(
                    vec![b"ABC".to_vec()],
                    Some(1),
                    Arc::new(Mutex::new(Vec::new())),
                )) as Box<dyn Transport>,
                _ => Box::new(ScriptedTransport::new(
                    vec![b"DEF".to_vec()],
                    None,
                    Arc::new(Mutex::new(Vec::new())),
                )) as Box<dyn Transport>,
            })
        }));
        let s = spawn_session(opts);
        assert!(wait_until(4000, || s.stats().rx_bytes >= 6));
        let st = s.stats();
        assert_eq!(st.rx_bytes, 6);
        assert_eq!(st.reconnects, 1, "断线后应恰好重连一次");
        let mut out = [0u8; 16];
        let rep = s.drain(&mut out);
        assert_eq!(rep.bytes, 6);
        assert_eq!(&out[..6], b"ABCDEF", "跨重连的字节序必须保持");
        s.stop();
    }

    #[test]
    fn open_failure_counts_reconnects() {
        let call = Arc::new(AtomicU32::new(0));
        let call2 = Arc::clone(&call);
        let opts = SessionOptions::new(Box::new(move || {
            if call2.fetch_add(1, Ordering::Relaxed) < 2 {
                Err(io::Error::new(ErrorKind::NotFound, "脚本:端口不存在"))
            } else {
                Ok(Box::new(ScriptedTransport::new(
                    vec![b"OK".to_vec()],
                    None,
                    Arc::new(Mutex::new(Vec::new())),
                )) as Box<dyn Transport>)
            }
        }));
        let s = spawn_session(opts);
        assert!(wait_until(5000, || s.stats().rx_bytes >= 2));
        let st = s.stats();
        assert_eq!(st.rx_bytes, 2);
        assert_eq!(st.reconnects, 2, "两次建连失败应计两次重连");
        s.stop();
    }

    #[test]
    fn write_goes_to_transport() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let w2 = Arc::clone(&writes);
        let opts = SessionOptions::new(Box::new(move || {
            Ok(Box::new(ScriptedTransport::new(vec![], None, Arc::clone(&w2))) as Box<dyn Transport>)
        }));
        let s = spawn_session(opts);
        assert!(wait_until(2000, || s.stats().connected));
        assert_eq!(s.write(b"AT+RST\r\n").unwrap(), 8);
        assert_eq!(s.stats().tx_bytes, 8);
        assert_eq!(*writes.lock().unwrap(), vec![b"AT+RST\r\n".to_vec()]);
        s.stop();
    }

    #[test]
    fn write_before_connect_fails() {
        // 工厂首次调用前 write 必须返回 NotConnected 而非 panic
        let gate = Arc::new(AtomicBool::new(false));
        let g2 = Arc::clone(&gate);
        let opts = SessionOptions::new(Box::new(move || {
            while !g2.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(20)); // 模拟慢建连
            }
            Ok(Box::new(ScriptedTransport::new(vec![], None, Arc::new(Mutex::new(Vec::new())))) as Box<dyn Transport>)
        }));
        let s = spawn_session(opts);
        thread::sleep(Duration::from_millis(50));
        let r = s.write(b"x");
        // 可能已连上(成功)也可能尚未(NotConnected),两者都必须是 Result 而非 panic
        assert!(r.is_ok() || r.unwrap_err().kind() == ErrorKind::NotConnected);
        s.stop();
    }

    #[test]
    fn stop_terminates_reader() {
        let opts = SessionOptions::new(Box::new(|| {
            Ok(Box::new(ScriptedTransport::new(
                vec![],
                None,
                Arc::new(Mutex::new(Vec::new())),
            )) as Box<dyn Transport>)
        }));
        let s = spawn_session(opts);
        thread::sleep(Duration::from_millis(100));
        assert!(s.stats().connected);
        s.stop();
        assert!(wait_until(2000, || !s.stats().connected), "stop 后 I/O 线程应退出置 disconnected");
    }
}
