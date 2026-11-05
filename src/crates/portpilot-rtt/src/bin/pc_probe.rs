// PortPilot · 复位运行后的执行位置诊断:PC 轨迹 + CB 魔数,判定 ROM 等待 vs 中断屏蔽
use probe_rs::MemoryInterface;
use probe_rs::Permissions;
use probe_rs::probe::list::Lister;
use std::time::Duration;

const CB: u64 = 0x3FC9_6AE4;
const CB_WR: u64 = CB + 24 + 12;

fn main() {
    probe_rs_espressif::register_plugin();
    let probes = Lister::new().list_all();
    let info = probes.iter().find(|p| p.vendor_id == 0x303a).expect("probe");
    let probe = info.open().expect("open");
    let mut session = probe.attach("esp32s3", Permissions::default()).expect("attach");

    // 复位并运行 core0
    {
        let mut core0 = session.core(0).expect("core0");
        match core0.reset_and_halt(Duration::from_millis(1000)) {
            Ok(i) => eprintln!("[pc] reset_and_halt PC={:#010x}", i.pc),
            Err(e) => eprintln!("[pc] reset_and_halt Err({e})"),
        }
        core0.run().expect("run");
    }
    // 恢复 core1
    {
        let mut core1 = session.core(1).expect("core1");
        eprintln!("[pc] core1 status: {:?}", core1.status());
        let _ = core1.run();
    }

    // 追踪 PC + wrOff + 魔数
    for round in 0..10 {
        {
            let mut core0 = session.core(0).expect("core0");
            match core0.status() {
                Ok(s) => {
                    if s.is_halted() {
                        eprintln!("[pc] r{round}: core0 HALTED");
                    } else {
                        // 不 halt,避免冻结;只读 wrOff 和魔数
                    }
                }
                Err(e) => eprintln!("[pc] r{round}: status Err({e})"),
            }
        }
        {
            let mut core0 = session.core(0).expect("core0");
            let mut w = [0u32; 1];
            let _ = MemoryInterface::read_32(&mut core0, CB_WR, &mut w);
            let mut id = [0u8; 16];
            let _ = MemoryInterface::read_8(&mut core0, CB, &mut id);
            eprintln!(
                "[pc] r{round}: wrOff={} id={:?}",
                w[0],
                String::from_utf8_lossy(&id)
            );
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}
