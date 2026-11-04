// PortPilot P3 · RTT 传输:第一步实证——调试探针枚举
// 预期:ESP32-S3 内建 USB-JTAG(VID 0x303A:PID 0x1001)应出现在列表中
use probe_rs::probe::list::Lister;
use probe_rs::probe::Probe;

fn main() {
    // probe-rs 0.28 API:Lister::new().list_all()(原 Probe::list_all 已迁移)
    let probes = Lister::new().list_all();
    if probes.is_empty() {
        println!("未发现任何调试探针。");
        println!("提示:ESP32-S3 内建 USB-JTAG 在 Windows 上可能需要 WinUSB 驱动;");
        println!("若 COM11(CDC 口)在位而 JTAG 接口缺席,尝试 Espressif 驱动或 Zadig 绑定 WinUSB。");
        return;
    }
    for p in &probes {
        println!(
            "探针: {} | VID={:04x} PID={:04x} | {}",
            p.identifier,
            p.vendor_id,
            p.product_id,
            p.probe_type()
        );
    }
}
