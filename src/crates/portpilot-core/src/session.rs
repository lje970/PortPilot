//! 会话:传输工厂 + I/O 线程(自动重连)+ 免分配环形缓冲 + 统计快照。
//!
//! UI 以固定帧率调用 [`SessionHandle::drain`],永不直接触碰串口;
//! I/O 线程内唯一堆分配发生在建连时刻,热路径(读→入环)零分配。

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
    pub reconnects: u32,
    pub connected: bool,
    /// 环形缓冲满导致的写侧自旋等待次数(>0 说明消费端跟不上)
    pub backpressure_events: u64,
    /// 到达时间戳环溢出(丢失的到达样本数,仅影响延迟统计精度)
    pub arrival_dropped: u64,
    pub ring_used: usize,
    pub ring_peak: usize,
}

pub struct SessionHandle {
    stop: Arc<AtomicBool>,
    ring: Arc<SpscRing>,
    arrivals: Arc<SpscRing>, // 每读块 8B LE u64(相对会话起始的毫秒)
    rx_bytes: Arc<AtomicU64>,
    reconnects: Arc<AtomicU32>,
    connected: Arc<AtomicBool>,
    backpressure: Arc<AtomicU64>,
    arrival_dropped: Arc<AtomicU64>,
    start: Arc<Mutex<Instant>>,
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

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 启动会话 I/O 线程。返回句柄后立即生效;`stop()` 令线程在 ≤ 读超时 + 重连间隔内退出。
pub fn spawn_session(opts: SessionOptions) -> SessionHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let ring = Arc::new(SpscRing::with_capacity(opts.ring_capacity));
    let arrivals = Arc::new(SpscRing::with_capacity(4096 * 8)); // 4096 个 u64
    let rx_bytes = Arc::new(AtomicU64::new(0));
    let reconnects = Arc::new(AtomicU32::new(0));
    let connected = Arc::new(AtomicBool::new(false));
    let backpressure = Arc::new(AtomicU64::new(0));
    let arrival_dropped = Arc::new(AtomicU64::new(0));
    let start = Arc::new(Mutex::new(Instant::now()));

    let mut factory = opts.factory;
    let delay = Duration::from_millis(opts.reconnect_delay_ms.max(50));
    let th_stop = Arc::clone(&stop);
    let th_ring = Arc::clone(&ring);
    let th_arr = Arc::clone(&arrivals);
    let th_rx = Arc::clone(&rx_bytes);
    let th_rec = Arc::clone(&reconnects);
    let th_conn = Arc::clone(&connected);
    let th_bp = Arc::clone(&backpressure);
    let th_adr = Arc::clone(&arrival_dropped);
    let th_start = Arc::clone(&start);

    thread::spawn(move || {
        let mut buf = vec![0u8; 8192]; // 唯一一次堆分配(线程栈上大缓冲会爆栈)
        let t0 = *th_start.lock().unwrap();
        let ms_since = |t0: Instant| t0.elapsed().as_secs_f64() * 1000.0;

        'outer: while !th_stop.load(Ordering::Relaxed) {
            // ---- 建连 ----
            let mut transport = match factory() {
                Ok(t) => {
                    th_conn.store(true, Ordering::Relaxed);
                    t
                }
                Err(_) => {
                    th_rec.fetch_add(1, Ordering::Relaxed);
                    th_conn.store(false, Ordering::Relaxed);
                    thread::sleep(delay);
                    continue 'outer;
                }
            };

            // ---- 读循环 ----
            'inner: loop {
                if th_stop.load(Ordering::Relaxed) {
                    transport.close();
                    th_conn.store(false, Ordering::Relaxed);
                    break 'outer;
                }
                match transport.read(&mut buf) {
                    Ok(0) => {} // 读超时,继续轮询
                    Ok(n) => {
                        // 入环:满则自旋等待消费端(背压),不丢数据
                        let mut pushed = 0usize;
                        while pushed < n {
                            let k = th_ring.push_slice(&buf[pushed..n]);
                            pushed += k;
                            if pushed < n {
                                th_bp.fetch_add(1, Ordering::Relaxed);
                                thread::sleep(Duration::from_micros(200));
                                if th_stop.load(Ordering::Relaxed) {
                                    transport.close();
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
                    }
                    Err(_) => {
                        // 断线:关闭并进入重连
                        transport.close();
                        th_conn.store(false, Ordering::Relaxed);
                        th_rec.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(delay);
                        continue 'outer;
                    }
                }
            }
        }
        th_conn.store(false, Ordering::Relaxed);
    });

    SessionHandle {
        stop,
        ring,
        arrivals,
        rx_bytes,
        reconnects,
        connected,
        backpressure,
        arrival_dropped,
        start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    /// 脚本化假传输:按预置块回放数据;可指定第 N 次 read 后断线。
    struct ScriptedTransport {
        chunks: Vec<Vec<u8>>,
        idx: usize,
        err_after_read: Option<usize>,
        reads: usize,
    }

    impl ScriptedTransport {
        fn new(chunks: Vec<Vec<u8>>, err_after_read: Option<usize>) -> Self {
            Self { chunks, idx: 0, err_after_read, reads: 0 }
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
        fn write(&mut self, _data: &[u8]) -> io::Result<usize> {
            Ok(0)
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
                    Some(1), // 第 2 次 read 即断线
                )) as Box<dyn Transport>,
                _ => Box::new(ScriptedTransport::new(vec![b"DEF".to_vec()], None))
                    as Box<dyn Transport>,
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
                Ok(Box::new(ScriptedTransport::new(vec![b"OK".to_vec()], None))
                    as Box<dyn Transport>)
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
    fn stop_terminates_reader() {
        let opts = SessionOptions::new(Box::new(|| {
            Ok(Box::new(ScriptedTransport::new(vec![], None)) as Box<dyn Transport>)
        }));
        let s = spawn_session(opts);
        thread::sleep(Duration::from_millis(100));
        assert!(s.stats().connected);
        s.stop();
        assert!(wait_until(2000, || !s.stats().connected), "stop 后 I/O 线程应退出置 disconnected");
    }
}
