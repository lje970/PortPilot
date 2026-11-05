// PortPilot P3 · RttTransport:SEGGER RTT 作为 ITransport 实现
// 设计要点(docs/04 §1 实测铁律):
//   1. 全程只走 MemoryInterface(读前临时停、读完即恢复的 halted_access 式轮询),
//      严禁 halt()/reset()/reset_and_halt() —— Xtensa resume 缺陷会冻结目标;
//   2. 空闲轮询返回 Ok(0) 前内睡 5ms(session 线程对 Ok(0) 无休眠,防 JTAG 忙旋);
//   3. attach 时起始 rd 取固件当前 rdOff,存量数据由首次 read() 自然追出;
//   4. 断线由 session 线程重连:工厂闭包重建(重 attach + 重扫描,约 1s)。
use portpilot_core::transport::Transport;
use probe_rs::MemoryInterface;
use probe_rs::Permissions;
use probe_rs::probe::list::Lister;
use std::io;
use std::sync::Once;
use std::time::Duration;

const MAGIC: &[u8; 11] = b"SEGGER RTT\0";
const IDLE_SLEEP_MS: u64 = 5;

/// 注册 Espressif 探针工厂(0.32 起 ESP 支持拆到独立 crate,只做一次)
fn ensure_plugin() {
    static ONCE: Once = Once::new();
    ONCE.call_once(probe_rs_espressif::register_plugin);
}

pub struct RttTransport {
    session: Option<probe_rs::Session>,
    /// up 环缓冲地址 / 容量
    buf_addr: u64,
    buf_size: u32,
    /// up[0].wrOff / rdOff 的内存地址
    wr_addr: u64,
    rd_addr: u64,
    /// 消费位置(与固件 rdOff 同步回写)
    rd: u32,
    /// down[0] 描述(可选下行;固件未定义时 size=0)
    down_buf: u64,
    down_size: u32,
    down_wr_addr: u64,
}

impl RttTransport {
    /// 打开探针、attach 芯片并扫描 RTT 控制块(约 1s)。
    /// `chip`:目标芯片名,当前支持 "esp32s3"(内置 USB-JTAG,VID 303A PID 1001)。
    pub fn open(chip: &str) -> io::Result<Self> {
        ensure_plugin();
        let (dram_base, dram_len) = match chip {
            "esp32s3" => (0x3FC8_8000u64, 512 * 1024u64),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("RTT 暂仅支持 esp32s3(内置 USB-JTAG),收到 {other:?}"),
                ))
            }
        };

        // 1) 枚举并打开探针(找不到/被占用 → 明确报错)
        let probes = Lister::new().list_all();
        let info = probes
            .iter()
            .find(|p| p.vendor_id == 0x303a)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "未找到 Espressif USB-JTAG 探针(303A:1001);请确认板子已插、WinUSB 驱动已装(scripts/fix_guid_params.ps1)",
                )
            })?;
        let probe = info.open().map_err(|e| {
            io::Error::other(format!("探针打开失败(被其他程序占用?): {e}"))
        })?;

        // 2) attach(不复位、不停核)
        let mut session = probe
            .attach(chip, Permissions::default())
            .map_err(|e| io::Error::other(format!("attach {chip} 失败: {e}")))?;

        // 3) 扫描 DRAM 找控制块(4KB 分块;映射空洞跳过)
        let mut cb_addr: Option<u64> = None;
        {
            let mut core = session.core(0).map_err(last_err)?;
            let mut chunk = vec![0u8; 4096];
            for base in (dram_base..dram_base + dram_len).step_by(4096) {
                if MemoryInterface::read_8(&mut core, base, &mut chunk).is_err() {
                    continue;
                }
                for w in 0..chunk.len().saturating_sub(MAGIC.len()) {
                    if &chunk[w..w + MAGIC.len()] == MAGIC {
                        cb_addr = Some(base + w as u64);
                        break;
                    }
                }
                if cb_addr.is_some() {
                    break;
                }
            }
        }
        let cb = cb_addr.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "DRAM 中未找到 SEGGER RTT 控制块(固件未内嵌 RTT?目标未运行?)",
            )
        })?;

        // 4) 解析控制块:id[16] maxUp@16 maxDown@20;
        //    up[0]@24: name@+0 buf@+4 size@+8 wr@+12 rd@+16;down[0]@48 同布局
        let (buf_addr, buf_size, rd) = {
            let mut core = session.core(0).map_err(last_err)?;
            let rd32 = |core: &mut probe_rs::Core<'_>, a: u64| -> io::Result<u32> {
                let mut w = [0u32; 1];
                MemoryInterface::read_32(core, a, &mut w).map_err(last_err)?;
                Ok(w[0])
            };
            let buf_addr = rd32(&mut core, cb + 24 + 4)? as u64;
            let buf_size = rd32(&mut core, cb + 24 + 8)?;
            let rd = rd32(&mut core, cb + 24 + 16)?;
            (buf_addr, buf_size, rd)
        };
        if buf_size == 0 || buf_size > 1024 * 1024 || buf_addr == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("RTT up 描述异常: buf={buf_addr:#x} size={buf_size}"),
            ));
        }
        let (down_buf, down_size) = {
            let mut core = session.core(0).map_err(last_err)?;
            let rd32 = |core: &mut probe_rs::Core<'_>, a: u64| -> io::Result<u32> {
                let mut w = [0u32; 1];
                MemoryInterface::read_32(core, a, &mut w).map_err(last_err)?;
                Ok(w[0])
            };
            (rd32(&mut core, cb + 48 + 4)? as u64, rd32(&mut core, cb + 48 + 8)?)
        };
        let rd32 = |core: &mut probe_rs::Core<'_>, a: u64| -> io::Result<u32> {
            let mut w = [0u32; 1];
            MemoryInterface::read_32(core, a, &mut w).map_err(last_err)?;
            Ok(w[0])
        };
        let _ = rd32; // (闭包保留给后续扩展;当前解析已在上面作用域完成)

        Ok(Self {
            session: Some(session),
            buf_addr,
            buf_size,
            wr_addr: cb + 24 + 12,
            rd_addr: cb + 24 + 16,
            rd,
            down_buf,
            down_size,
            down_wr_addr: cb + 48 + 12,
        })
    }
}

fn last_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("JTAG 内存访问失败: {e}"))
}

impl Transport for RttTransport {
    /// 轮询 up 环:有新数据返回字节;无数据内睡 5ms 返回 Ok(0)(超时语义)。
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "RTT 会话已关闭"))?;
        let mut core = session.core(0).map_err(last_err)?;

        let mut w = [0u32; 1];
        MemoryInterface::read_32(&mut core, self.wr_addr, &mut w).map_err(last_err)?;
        let new_wr = w[0];
        let avail = new_wr.wrapping_sub(self.rd) % self.buf_size;
        if avail == 0 {
            drop(core);
            std::thread::sleep(Duration::from_millis(IDLE_SLEEP_MS));
            return Ok(0);
        }
        // 单次读:不超过调用缓冲、不跨环回绕
        let pos = (self.rd % self.buf_size) as usize;
        let n = (avail as usize)
            .min(buf.len())
            .min(self.buf_size as usize - pos);
        MemoryInterface::read_8(&mut core, self.buf_addr + pos as u64, &mut buf[..n])
            .map_err(last_err)?;
        self.rd = (self.rd + n as u32) % self.buf_size;
        // 回写消费位置(firmware 侧不读,回写仅为语义完整/多读端兼容)
        let w4 = [self.rd, 0, 0, 0];
        let _ = MemoryInterface::write_32(&mut core, self.rd_addr, &w4);
        Ok(n)
    }

    /// 下行:固件定义了 down buffer(size>0)才可用,写入并推进 wrOff。
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.down_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "固件未定义 RTT down buffer,下行不可用",
            ));
        }
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "RTT 会话已关闭"))?;
        let mut core = session.core(0).map_err(last_err)?;
        let rd32 = |core: &mut probe_rs::Core<'_>, a: u64| -> io::Result<u32> {
            let mut w = [0u32; 1];
            MemoryInterface::read_32(core, a, &mut w).map_err(last_err)?;
            Ok(w[0])
        };
        let wr = rd32(&mut core, self.down_wr_addr)?;
        // 剩余空间:down 环以 wr 追 rd(host 写,firmware 读);这里保守取不回绕一段
        let rd = rd32(&mut core, self.down_wr_addr + 4)?;
        let space = self.down_size.wrapping_sub(wr.wrapping_sub(rd)) % self.down_size;
        if space == 0 {
            std::thread::sleep(Duration::from_millis(IDLE_SLEEP_MS));
            return Ok(0);
        }
        let pos = (wr % self.down_size) as usize;
        let n = (space as usize).min(data.len()).min(self.down_size as usize - pos);
        MemoryInterface::write_8(&mut core, self.down_buf + pos as u64, &data[..n])
            .map_err(last_err)?;
        let new_wr = (wr + n as u32) % self.down_size;
        let w4 = [new_wr, 0, 0, 0];
        MemoryInterface::write_32(&mut core, self.down_wr_addr, &w4).map_err(last_err)?;
        Ok(n)
    }

    fn close(&mut self) {
        // 丢掉 session:drop 会释放探针句柄;不复位目标
        self.session = None;
    }
}
