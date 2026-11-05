// PortPilot · 直读 RTT 控制块诊断:按 ELF 符号地址读 rttCB,验证目标是否在运行
use probe_rs::MemoryInterface;
use probe_rs::Permissions;
use probe_rs::probe::list::Lister;

const CB_ADDR: u64 = 0x3FC9_6AE4; // rttCB (bss)
const UPBUF_ADDR: u64 = 0x3FC9_6B2C; // upBuf (bss)

fn main() {
    probe_rs_espressif::register_plugin(); // 0.32 ESP 工厂显式注册
    let probes = Lister::new().list_all();
    let info = match probes.iter().find(|p| p.vendor_id == 0x303a) {
        Some(p) => p,
        None => {
            eprintln!("[diag] 未找到探针");
            return;
        }
    };
    let probe = match info.open() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[diag] 打开失败: {e}");
            return;
        }
    };
    let mut session = match probe.attach("esp32s3", Permissions::default()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[diag] attach 失败: {e}");
            return;
        }
    };

    // 连续读 3 次,间隔 300ms,看 wrOff 是否变化(=固件在写)
    for round in 0..3 {
        let mut core = session.core(0).expect("core0");
        if round == 0 {
            // 若核心处于 halted(上次会话遗留),恢复运行;不要主动 halt!
            match core.status() {
                Ok(s) if s.is_halted() => {
                    eprintln!("[diag] 核心处于 halted,执行 run() 恢复 ...");
                    if let Err(e) = core.run() {
                        eprintln!("[diag] run() 失败: {e}");
                    }
                }
                Ok(s) => eprintln!("[diag] 核心状态: {s:?}"),
                Err(e) => eprintln!("[diag] status 查询失败: {e}"),
            }
        }
        let mut id = [0u8; 16];
        let mut wr = [0u32; 1];
        let mut rd = [0u32; 1];
        let mut head = [0u8; 48];
        if let Err(e) = MemoryInterface::read_8(&mut core, CB_ADDR, &mut id) {
            eprintln!("[diag] round{round} 读 id 失败: {e}");
            continue;
        }
        let _ = MemoryInterface::read_8(&mut core, CB_ADDR + 24, &mut head); // up[0] 描述符
        let _ = MemoryInterface::read_32(&mut core, CB_ADDR + 24 + 12, &mut wr); // wrOff
        let _ = MemoryInterface::read_32(&mut core, CB_ADDR + 24 + 16, &mut rd); // rdOff
        let id_s = String::from_utf8_lossy(&id);
        eprintln!(
            "[diag] round{round}: id={id_s:?} up_desc={:02x?} wrOff={} rdOff={}",
            &head[..24],
            wr[0],
            rd[0]
        );
        drop(core);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    // 附:读 upBuf 前 64 字节 + flash 映射区 app 头
    let mut core = session.core(0).expect("core0");
    let mut buf = [0u8; 64];
    match MemoryInterface::read_8(&mut core, UPBUF_ADDR, &mut buf) {
        Ok(_) => eprintln!("[diag] upBuf 前64B: {}", String::from_utf8_lossy(&buf)),
        Err(e) => eprintln!("[diag] upBuf 读失败: {e}"),
    }
    // flash 映射:0x3C000000=flash偏移0,app@0x10000 → 0x3C010000,头应 E9
    let mut fhead = [0u8; 16];
    match MemoryInterface::read_8(&mut core, 0x3C01_0000, &mut fhead) {
        Ok(_) => eprintln!(
            "[diag] flash映射@0x3C010000: {:02x?} (首字节 0xE9=合法app镜像)",
            fhead
        ),
        Err(e) => eprintln!("[diag] flash映射读失败: {e}"),
    }
    // 在 flash 映射区前 256KB 找 "SEGGER RTT"(确认固件内容在 flash 里)
    let mut fchunk = vec![0u8; 4096];
    let mut found = None;
    for base in (0x3C00_0000..0x3C04_0000).step_by(4096) {
        if MemoryInterface::read_8(&mut core, base, &mut fchunk).is_err() {
            continue;
        }
        for w in 0..fchunk.len().saturating_sub(11) {
            if &fchunk[w..w + 10] == b"SEGGER RTT" {
                found = Some(base + w as u64);
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    match found {
        Some(a) => eprintln!("[diag] flash 内找到 'SEGGER RTT' @ {a:#010x} ✅"),
        None => eprintln!("[diag] flash 前256KB 未找到 'SEGGER RTT' ❌(固件没进 flash?)"),
    }
}
