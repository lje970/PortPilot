// PortPilot · Xtensa run() 行为验证:core0/core1 双核恢复测试
use probe_rs::MemoryInterface;
use probe_rs::Permissions;
use probe_rs::probe::list::Lister;
use std::time::Duration;

const CB_WR: u64 = 0x3FC9_6AE4 + 24 + 12; // rttCB.up[0].wrOff

fn main() {
    probe_rs_espressif::register_plugin(); // 0.32 ESP 工厂显式注册
    let probes = Lister::new().list_all();
    let info = probes.iter().find(|p| p.vendor_id == 0x303a).expect("probe");
    let probe = info.open().expect("open");
    let mut session = probe.attach("esp32s3", Permissions::default()).expect("attach");

    fn wr(session: &mut probe_rs::Session, tag: &str) -> u32 {
        let mut core = session.core(0).expect("core0");
        let mut w = [0u32; 1];
        let _ = MemoryInterface::read_32(&mut core, CB_WR, &mut w);
        eprintln!("[probe] {tag} wrOff={}", w[0]);
        w[0]
    }

    // 阶段1:attach 后直读(不 run)
    let mut prev = 0u32;
    for i in 0..3 {
        prev = wr(&mut session, &format!("attach后#{i}"));
        std::thread::sleep(Duration::from_millis(200));
    }

    // 阶段2:复位并运行 core0,再恢复 core1
    {
        let mut core0 = session.core(0).expect("core0");
        match core0.reset_and_halt(Duration::from_millis(1000)) {
            Ok(info) => eprintln!("[probe] reset_and_halt Ok, PC={:#010x}", info.pc),
            Err(e) => eprintln!("[probe] reset_and_halt Err({e})"),
        }
        match core0.run() {
            Ok(_) => eprintln!("[probe] core0 run() = Ok"),
            Err(e) => eprintln!("[probe] core0 run() = Err({e})"),
        }
    }
    {
        let mut core1 = session.core(1).expect("core1");
        match core1.status() {
            Ok(s) => eprintln!("[probe] core1 status: {s:?}"),
            Err(e) => eprintln!("[probe] core1 status Err({e})"),
        }
        match core1.run() {
            Ok(_) => eprintln!("[probe] core1 run() = Ok"),
            Err(e) => eprintln!("[probe] core1 run() = Err({e})"),
        }
    }

    std::thread::sleep(Duration::from_millis(1500));
    let mut ok = false;
    let mut last = 0u32;
    for i in 0..8 {
        let v = wr(&mut session, &format!("复位后#{i}"));
        if last != 0 && v != last {
            eprintln!("[probe] ✅ wrOff 在推进!");
            ok = true;
        }
        last = v;
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = prev;
    if !ok {
        eprintln!("[probe] ❌ 复位后 wrOff 仍冻结");
    }
}
