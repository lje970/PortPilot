// PortPilot · pplog v1 会话日志格式
// 设计目标:把"捕获"与"解读"分离——原始字节流连同毫秒时间戳自描述存档,
// 解析/回放是对存档的透镜(见 docs/03 §8.11)。
//
// 格式(v1):
//   头部:  "PPLG"(4B) | version u8 = 1 | 保留 3B(0) | meta_len u32 LE | meta JSON(UTF-8)
//   记录:  [ms u64 LE][dir u8][len u32 LE][bytes]  × N
//          ms = 相对会话起始的毫秒;dir: 0=RX, 1=TX
// 兼容:  旧版裸字节日志(无 magic)由 is_pplog 判别,调用方走 legacy 路径。

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Instant;

pub const MAGIC: &[u8; 4] = b"PPLG";
pub const VERSION: u8 = 1;
pub const DIR_RX: u8 = 0;
pub const DIR_TX: u8 = 1;

/// 会话元数据(头部 JSON)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PplogMeta {
    pub kind: String,   // serial | tcp_client | tcp_server | udp | loop | rtt
    pub port: String,   // COM 口 / 地址 / 芯片名
    pub baud: u32,
    pub encoding: String, // utf8 | gbk
    pub created_utc: String,
}

/// 一条记录(ms 相对会话起始;dir 0=RX 1=TX)。
#[derive(Debug, Clone, PartialEq)]
pub struct PplogRecord {
    pub ms: u64,
    pub dir: u8,
    pub data: Vec<u8>,
}

/// 判别文件是否为 pplog 格式(否则视为旧版裸字节流)。
pub fn is_pplog(path: &Path) -> bool {
    std::fs::File::open(path)
        .and_then(|mut f| {
            let mut m = [0u8; 4];
            f.read_exact(&mut m)?;
            Ok(m == *MAGIC)
        })
        .unwrap_or(false)
}

/// 写入器:record_bytes 用统一小缓冲,避免每条记录分配。
pub struct PplogWriter {
    file: std::fs::File,
    start: Instant,
    buf: Vec<u8>,
}

impl PplogWriter {
    pub fn create(path: &Path, meta: &PplogMeta) -> io::Result<Self> {
        let mut file = std::fs::File::create(path)?;
        let meta_json = serde_json::to_vec(meta).map_err(io::Error::other)?;
        let mut head = Vec::with_capacity(12 + meta_json.len());
        head.extend_from_slice(MAGIC);
        head.push(VERSION);
        head.extend_from_slice(&[0u8; 3]); // 保留
        head.extend_from_slice(&(meta_json.len() as u32).to_le_bytes());
        head.extend_from_slice(&meta_json);
        file.write_all(&head)?;
        Ok(Self { file, start: Instant::now(), buf: Vec::with_capacity(4096) })
    }

    /// 追加一条记录(ms 自动取相对会话起始)。
    pub fn write_record(&mut self, dir: u8, data: &[u8]) -> io::Result<()> {
        self.write_record_at(self.start.elapsed().as_millis() as u64, dir, data)
    }

    /// 显式指定毫秒(回放/导入场景)。
    pub fn write_record_at(&mut self, ms: u64, dir: u8, data: &[u8]) -> io::Result<()> {
        self.buf.clear();
        self.buf.extend_from_slice(&ms.to_le_bytes());
        self.buf.push(dir);
        self.buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(data);
        self.file.write_all(&self.buf)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// 读取器:逐条产出记录;尾部残缺记录(写一半崩溃)静默忽略。
pub struct PplogReader {
    file: std::fs::File,
}

impl PplogReader {
    pub fn open(path: &Path) -> io::Result<(Self, PplogMeta)> {
        let mut file = std::fs::File::open(path)?;
        let mut head = [0u8; 12];
        file.read_exact(&mut head)?;
        if &head[0..4] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "缺少 PPLG magic"));
        }
        if head[4] != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("pplog 版本不支持: {}", head[4]),
            ));
        }
        let meta_len = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
        if meta_len > 1 << 20 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "meta 过长"));
        }
        let mut mj = vec![0u8; meta_len];
        file.read_exact(&mut mj)?;
        let meta: PplogMeta =
            serde_json::from_slice(&mj).map_err(io::Error::other)?;
        Ok((Self { file }, meta))
    }

    /// 读下一条记录;Ok(None) = 正常结束,尾部残缺不报错。
    pub fn next_record(&mut self) -> io::Result<Option<PplogRecord>> {
        let mut hdr = [0u8; 13];
        match self.file.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let ms = u64::from_le_bytes(hdr[0..8].try_into().unwrap());
        let dir = hdr[8];
        let len = u32::from_le_bytes(hdr[9..13].try_into().unwrap()) as usize;
        if len > 64 * 1024 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "记录长度非法"));
        }
        let mut data = vec![0u8; len];
        match self.file.read_exact(&mut data) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None), // 尾部残缺
            Err(e) => return Err(e),
        }
        Ok(Some(PplogRecord { ms, dir, data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("pp_test_{}_{}.pplog", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn meta() -> PplogMeta {
        PplogMeta {
            kind: "serial".into(),
            port: "COM11".into(),
            baud: 256000,
            encoding: "utf8".into(),
            created_utc: "2026-11-08T00:00:00Z".into(),
        }
    }

    #[test]
    fn roundtrip_preserves_records_and_meta() {
        let path = tmp_path("roundtrip");
        let m = meta();
        {
            let mut w = PplogWriter::create(&path, &m).unwrap();
            w.write_record(DIR_RX, b"hello ").unwrap();
            w.write_record(DIR_TX, b"AT\r\n").unwrap();
            w.write_record_at(1500, DIR_RX, &[0xaa, 0xff, 0x03, 0x00]).unwrap();
            w.write_record(DIR_RX, &[]).unwrap(); // 空载荷合法
            w.flush().unwrap();
        }
        assert!(is_pplog(&path));
        let (mut r, m2) = PplogReader::open(&path).unwrap();
        assert_eq!(m2, m);
        let r1 = r.next_record().unwrap().unwrap();
        assert_eq!((r1.ms <= 50, r1.dir, r1.data.as_slice()), (true, DIR_RX, b"hello ".as_slice()));
        let r2 = r.next_record().unwrap().unwrap();
        assert_eq!((r2.dir, r2.data.as_slice()), (DIR_TX, b"AT\r\n".as_slice()));
        let r3 = r.next_record().unwrap().unwrap();
        assert_eq!((r3.ms, r3.data.as_slice()), (1500, [0xaa, 0xff, 0x03, 0x00].as_slice()));
        let r4 = r.next_record().unwrap().unwrap();
        assert!(r4.data.is_empty());
        assert!(r.next_record().unwrap().is_none()); // 正常 EOF
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_raw_file_is_detected() {
        let path = tmp_path("legacy");
        std::fs::write(&path, b"\x12\x34raw bytes without magic").unwrap();
        assert!(!is_pplog(&path));
        assert!(PplogReader::open(&path).is_err()); // 旧格式走调用方 legacy 路径
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncated_tail_is_silently_ignored() {
        let path = tmp_path("trunc");
        {
            let mut w = PplogWriter::create(&path, &meta()).unwrap();
            w.write_record(DIR_RX, b"complete").unwrap();
            w.flush().unwrap();
            // 模拟写一半崩溃:追加残缺记录头+半截数据
            w.file.write_all(&[0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, DIR_RX, 0x08, 0x00, 0x00, 0x00]).unwrap();
            w.file.write_all(&[0x01, 0x02]).unwrap();
        }
        let (mut r, _) = PplogReader::open(&path).unwrap();
        let rec = r.next_record().unwrap().unwrap();
        assert_eq!(rec.data.as_slice(), b"complete");
        assert!(r.next_record().unwrap().is_none()); // 残缺被忽略,不报错
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn large_record_roundtrip() {
        let path = tmp_path("large");
        let big = vec![0x55u8; 300 * 1024]; // 大于典型一次泵批
        {
            let mut w = PplogWriter::create(&path, &meta()).unwrap();
            w.write_record(DIR_RX, &big).unwrap();
        }
        let (mut r, _) = PplogReader::open(&path).unwrap();
        let rec = r.next_record().unwrap().unwrap();
        assert_eq!(rec.data.len(), big.len());
        assert_eq!(rec.data, big);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_version_rejected() {
        let path = tmp_path("badver");
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.push(9); // 未知版本
        b.extend_from_slice(&[0u8; 3]);
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(b"{}");
        std::fs::write(&path, &b).unwrap();
        assert!(PplogReader::open(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
