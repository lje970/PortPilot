// PortPilot P3 · RTT 链路验证:probe-rs attach + RAM 扫描 "SEGGER RTT" + 读 up-buffer
// 配套固件:bench/mcu_firmware/esp32_rtt_pio(每 200ms 写 "Hello from RTT #N")
// API 口径(probe-rs 0.28):session.core(0) -> Core<'_>;内存读写走 MemoryInterface
// trait(read_8/write_8/read_32/write_32,均 &mut self)。
// 类型约定:环形指针 wr/rd/buf_size 用 u32(SEGGER 规格);内存地址用 u64。
use probe_rs::MemoryInterface;
use probe_rs::Permissions;
use probe_rs::probe::list::Lister;
use std::time::{Duration, Instant};

const DRAM_BASE: u64 = 0x3FC8_8000; // ESP32-S3 数据 RAM 起点
const SCAN_LEN: u64 = 512 * 1024;
const MAGIC: &[u8; 11] = b"SEGGER RTT\0";

fn main() {
    probe_rs_espressif::register_plugin(); // 0.32 ESP 工厂显式注册
    // 0) 日志:nusb/probe-rs 的 debug 输出走 log crate,RUST_LOG=debug 可见
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();

    // 1) 探针枚举
    let probes = Lister::new().list_all();
    let info = match probes.iter().find(|p| p.vendor_id == 0x303a) {
        Some(p) => p,
        None => {
            eprintln!("[rtt] 未找到 Espressif 探针(303a:1001)");
            return;
        }
    };
    eprintln!("[rtt] 探针: {}", info.identifier);

    // 2) 打开探针并 attach(不打断运行中的目标)
    let probe = match info.open() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[rtt] 打开探针失败: {e}");
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                eprintln!("  原因: {s}");
                src = std::error::Error::source(s);
            }
            return;
        }
    };
    let mut session = match probe.attach("esp32s3", Permissions::default()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[rtt] attach 失败: {e}");
            return;
        }
    };
    eprintln!("[rtt] attach 成功(esp32s3)");

    // 3) 扫描 DRAM 找 "SEGGER RTT" 魔数(MemoryInterface::read_8,4KB 分块)
    let mut cb_addr: Option<u64> = None;
    let mut chunk8 = vec![0u8; 4096];
    let scan_start = Instant::now();
    {
        let mut core = session.core(0).expect("core0");
        for base in (DRAM_BASE..DRAM_BASE + SCAN_LEN).step_by(4096) {
            if MemoryInterface::read_8(&mut core, base, &mut chunk8).is_err() {
                continue; // 映射空洞
            }
            for w in 0..chunk8.len() {
                if chunk8[w] == b'S' && w + 11 <= chunk8.len() && &chunk8[w..w + 11] == MAGIC {
                    cb_addr = Some(base + w as u64);
                    break;
                }
            }
            if cb_addr.is_some() {
                break;
            }
        }
    }
    let cb_addr = match cb_addr {
        Some(a) => {
            eprintln!(
                "[rtt] RTT 控制块 @ {a:#010x} ✅(扫描耗时 {:.1}s)",
                scan_start.elapsed().as_secs_f32()
            );
            a
        }
        None => {
            eprintln!("[rtt] 未找到 RTT 控制块(固件缺 RTT?目标未运行?)");
            // 诊断:打印 DRAM 起始 32 字节 + 统计读失败块数
            let mut core = session.core(0).expect("core0");
            let mut head = [0u8; 32];
            match MemoryInterface::read_8(&mut core, DRAM_BASE, &mut head) {
                Ok(_) => eprintln!("[rtt] DRAM@{DRAM_BASE:#x}: {:02x?}", head),
                Err(e) => eprintln!("[rtt] DRAM 读失败: {e}"),
            }
            let mut fail = 0u32;
            let mut ok = 0u32;
            for base in (DRAM_BASE..DRAM_BASE + SCAN_LEN).step_by(4096) {
                if MemoryInterface::read_8(&mut core, base, &mut chunk8).is_ok() {
                    ok += 1;
                } else {
                    fail += 1;
                }
            }
            eprintln!("[rtt] 扫描统计: 读成功 {ok} 块 / 读失败 {fail} 块");
            return;
        }
    };

    // 4) 读控制块字段(id[16] maxUp@16 maxDown@20;up[0]: name@24 buf@28 size@32 wr@36 rd@40)
    let mut core = session.core(0).expect("core0");
    fn rd32(core: &mut probe_rs::Core<'_>, addr: u64) -> u32 {
        let mut w = [0u32; 1];
        MemoryInterface::read_32(core, addr, &mut w).unwrap();
        w[0]
    }
    let buf_ptr = rd32(&mut core, cb_addr + 28) as u64;
    let buf_size = rd32(&mut core, cb_addr + 32);
    eprintln!("[rtt] up-buffer @ {buf_ptr:#010x} size={buf_size}");

    // 5) 起始追读:attach 时环内已有的存量数据(rd→wr 之间)全部读出。
    //    (Xtensa 上 attach 冻结目标后无增量,存量追读是拿到数据的关键;
    //     ARM 上则与后续实时增量衔接。)
    let mut wr = rd32(&mut core, cb_addr + 36);
    let mut rd = rd32(&mut core, cb_addr + 40);
    let mut carry = Vec::new();
    let mut total: u32 = 0;
    {
        let avail0 = wr.wrapping_sub(rd) % buf_size;
        if avail0 > 0 {
            let mut fresh = Vec::new();
            let mut avail = avail0;
            while avail > 0 {
                let pos = (rd % buf_size) as usize;
                let n = avail.min(buf_size - pos as u32) as usize;
                let mut tmp = vec![0u8; n];
                MemoryInterface::read_8(&mut core, buf_ptr + pos as u64, &mut tmp).unwrap();
                fresh.extend_from_slice(&tmp);
                rd = (rd + n as u32) % buf_size;
                avail -= n as u32;
            }
            let w4 = [rd, 0u32, 0u32, 0u32];
            MemoryInterface::write_32(&mut core, cb_addr + 40, &w4).unwrap();
            total += fresh.len() as u32;
            carry.extend_from_slice(&fresh);
            eprintln!("[rtt] 起始追读 {avail0}B(存量日志)");
        }
    }

    // 6) 跟随 WrOff 读 5 秒:每轮读环内新增字节,打印完整行,并回写 RdOff
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let new_wr = rd32(&mut core, cb_addr + 36);
        if new_wr == wr {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let mut avail: u32 = new_wr.wrapping_sub(wr) % buf_size;
        let mut fresh = Vec::new();
        while avail > 0 {
            let pos = (rd % buf_size) as usize;
            let n = avail.min((buf_size - pos as u32)) as usize;
            let mut tmp = vec![0u8; n];
            MemoryInterface::read_8(&mut core, buf_ptr + pos as u64, &mut tmp).unwrap();
            fresh.extend_from_slice(&tmp);
            rd = (rd + n as u32) % buf_size;
            avail -= n as u32;
        }
        // 回写 RdOff(消费确认)
        let w4 = [rd, 0u32, 0u32, 0u32];
        MemoryInterface::write_32(&mut core, cb_addr + 40, &w4).unwrap();
        wr = new_wr;
        total += fresh.len() as u32;

        carry.extend_from_slice(&fresh);
        while let Some(pos) = carry.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&carry[..pos]).into_owned();
            carry.drain(..=pos);
            if !line.trim().is_empty() {
                println!("[RTT] {line}");
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!("[rtt] 完成,共读 {total}B");
}
