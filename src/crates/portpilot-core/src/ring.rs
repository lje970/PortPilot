//! 无锁单生产者/单消费者(SPSC)字节环形缓冲 —— I/O 线程与 UI 之间的唯一数据通道。
//!
//! 容量必须为 2 的幂;head/tail 用原子序号(永不回绕,取模寻址)。
//! 写满时 `push_slice` 只推入能放下的部分并返回实际字节数,调用方自旋重试(背压),
//! 不丢数据、不分配内存。仅在单写单读线程下使用。
//!
//! 安全性:`buf` 用 `UnsafeCell` 提供内部可变性(共享为 Arc 后写侧 &self 写入)。
//! Soundness 依赖 SPSC 纪律:写者只碰 head 之后区域、读者只碰 tail 之后区域,
//! 两侧通过 head/tail 的 Release/Acquire 序建立 happens-before,永不交叉。

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub struct SpscRing {
    buf: UnsafeCell<Box<[u8]>>,
    mask: usize,
    head: AtomicUsize, // 写游标(仅写线程改)
    tail: AtomicUsize, // 读游标(仅读线程改)
    peak_used: AtomicUsize,
    total_pushed: AtomicU64,
}

// SAFETY:单写单读纪律 + 原子序号同步,见模块注释。
unsafe impl Sync for SpscRing {}

impl SpscRing {
    pub fn with_capacity(cap_pow2: usize) -> Self {
        assert!(cap_pow2.is_power_of_two(), "容量必须是 2 的幂");
        Self {
            buf: UnsafeCell::new(vec![0u8; cap_pow2].into_boxed_slice()),
            mask: cap_pow2 - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            peak_used: AtomicUsize::new(0),
            total_pushed: AtomicU64::new(0),
        }
    }

    #[inline]
    fn used_between(head: usize, tail: usize) -> usize {
        head - tail
    }

    /// 推入尽可能多的字节,返回实际推入数(缓冲满时 < data.len())。
    pub fn push_slice(&self, data: &[u8]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let free = self.capacity() - Self::used_between(head, tail);
        let n = data.len().min(free);
        if n == 0 {
            return 0;
        }
        // SAFETY:写区间 [head, head+n) 与读区间 [tail, head) 不相交(SPSC 纪律)
        let buf = unsafe { &mut *self.buf.get() };
        let pos = head & self.mask;
        let first = (self.capacity() - pos).min(n);
        buf[pos..pos + first].copy_from_slice(&data[..first]);
        if n > first {
            buf[..n - first].copy_from_slice(&data[first..n]);
        }
        self.head.store(head + n, Ordering::Release);
        self.total_pushed.fetch_add(n as u64, Ordering::Relaxed);
        let used = Self::used_between(head + n, tail);
        self.peak_used.fetch_max(used, Ordering::Relaxed);
        n
    }

    /// 取出尽可能多的字节,返回实际取出数。
    pub fn pop_slice(&self, out: &mut [u8]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let used = Self::used_between(head, tail);
        let n = out.len().min(used);
        if n == 0 {
            return 0;
        }
        // SAFETY:读区间 [tail, tail+n) 与写区间 [tail, head) 相同子集,由 head 的 Acquire 保证可见
        let buf = unsafe { &*self.buf.get() };
        let pos = tail & self.mask;
        let first = (self.capacity() - pos).min(n);
        out[..first].copy_from_slice(&buf[pos..pos + first]);
        if n > first {
            out[first..n].copy_from_slice(&buf[..n - first]);
        }
        self.tail.store(tail + n, Ordering::Release);
        n
    }

    /// 当前积压字节数(诊断用)。
    pub fn used(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        Self::used_between(head, tail)
    }

    pub fn peak_used(&self) -> usize {
        self.peak_used.load(Ordering::Relaxed)
    }

    pub fn total_pushed(&self) -> u64 {
        self.total_pushed.load(Ordering::Relaxed)
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn push_pop_basic() {
        let r = SpscRing::with_capacity(64);
        assert_eq!(r.push_slice(b"hello"), 5);
        let mut out = [0u8; 16];
        assert_eq!(r.pop_slice(&mut out), 5);
        assert_eq!(&out[..5], b"hello");
    }

    #[test]
    fn wraparound() {
        let r = SpscRing::with_capacity(8);
        for i in 0..32u8 {
            assert_eq!(r.push_slice(&[i; 4]), 4);
            let mut out = [0u8; 4];
            assert_eq!(r.pop_slice(&mut out), 4);
            assert_eq!(out, [i; 4]);
        }
    }

    #[test]
    fn full_buffer_backpressure() {
        let r = SpscRing::with_capacity(8);
        assert_eq!(r.push_slice(&[1u8; 8]), 8); // 满
        assert_eq!(r.push_slice(&[2u8; 1]), 0); // 推不进,背压
        let mut out = [0u8; 4];
        assert_eq!(r.pop_slice(&mut out), 4);
        assert_eq!(out, [1u8; 4]);
        assert_eq!(r.push_slice(&[2u8; 4]), 4);
        let mut out2 = [0u8; 8];
        assert_eq!(r.pop_slice(&mut out2), 8);
        assert_eq!(&out2[..4], &[1u8; 4]);
        assert_eq!(&out2[4..], &[2u8; 4]);
    }

    #[test]
    fn spsc_concurrent_byte_exact() {
        // 单写单读并发:1MB 数据逐字节校验(覆盖环形分段拷贝与回绕路径)
        const TOTAL: usize = 1 << 20;
        let r = Arc::new(SpscRing::with_capacity(4096));
        let w = Arc::clone(&r);
        let writer = thread::spawn(move || {
            let mut seq = [0u8; 256];
            let mut sent = 0usize;
            let mut i = 0u8;
            while sent < TOTAL {
                for b in seq.iter_mut() {
                    *b = i;
                    i = i.wrapping_add(1);
                }
                let mut chunk = &seq[..];
                while !chunk.is_empty() {
                    let n = w.push_slice(chunk);
                    chunk = &chunk[n..];
                    if n == 0 {
                        thread::yield_now();
                    }
                }
                sent += seq.len();
            }
        });
        let reader = thread::spawn(move || {
            let mut out = [0u8; 333]; // 非 2 幂读块,覆盖环形分段拷贝
            let mut got = 0usize;
            let mut expect = 0u8;
            while got < TOTAL {
                let n = r.pop_slice(&mut out);
                for &b in &out[..n] {
                    assert_eq!(b, expect, "字节序错位 @{}", got);
                    expect = expect.wrapping_add(1);
                }
                got += n;
                if n == 0 {
                    thread::yield_now();
                }
            }
        });
        writer.join().unwrap();
        reader.join().unwrap();
    }
}
