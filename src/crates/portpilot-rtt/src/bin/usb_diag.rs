// PortPilot P3 · USB 诊断:复现 EspJtag 打开流程,打印每一步的真实错误
// nusb 版本与 probe-rs 0.28 锁定的一致(0.1.14)
use nusb;

fn main() {
    let devices = nusb::list_devices().expect("list_devices");
    let mut found = None;
    for d in devices {
        if d.vendor_id() == 0x303a && d.product_id() == 0x1001 {
            println!(
                "发现设备: VID={:04x} PID={:04x} | 驱动={:?} | 路径={}",
                d.vendor_id(),
                d.product_id(),
                d.driver(),
                d.instance_id().to_string_lossy()
            );
            found = Some(d);
        }
    }
    let device = match found {
        Some(d) => d,
        None => {
            println!("!! 未找到 303a:1001 设备(板子 USB 未插?)");
            return;
        }
    };

    println!("== 尝试 device.open() ==");
    let handle = match device.open() {
        Ok(h) => h,
        Err(e) => {
            println!("!! open 失败: {e:?}");
            return;
        }
    };

    println!("== 尝试 claim MI_02(JTAG 接口)==");
    let iface = match handle.claim_interface(2) {
        Ok(i) => i,
        Err(e) => {
            println!("!! claim MI_02 失败: {e:?}");
            return;
        }
    };
    println!("claim 成功 —— 驱动绑定正常,可用 probe-rs 打开");

    // 尝试 claim MI_00(CDC)对比:probe-rs EspJtag 可能也要碰它
    drop(iface);
    match handle.claim_interface(0) {
        Ok(_) => println!("claim MI_00 成功"),
        Err(e) => println!("claim MI_00 失败: {e:?}"),
    }
}
