// PortPilot · JTAG 烧录工具:probe-rs 直接对 esp32s3 下载 ELF 并复位运行
// 用法: jtag_flash.exe <path-to-firmware.elf>
use probe_rs::flashing::{DownloadOptions, ElfLoader};
use probe_rs::{Permissions, Session};
use std::time::Duration;

fn main() {
    probe_rs_espressif::register_plugin(); // 0.32 ESP 工厂显式注册
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: jtag_flash <firmware.elf>");
        std::process::exit(1);
    }
    let elf = &args[1];

    // 1) 打开探针并 attach 到 esp32s3
    eprintln!("[flash] attach esp32s3 ...");
    let probe = match probe_rs::probe::list::Lister::new().list_all().iter().find(|p| p.vendor_id == 0x303a) {
        Some(p) => match p.open() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[flash] 探针打开失败: {e}");
                std::process::exit(1);
            }
        },
        None => {
            eprintln!("[flash] 未找到 Espressif 探针(303a:1001)");
            std::process::exit(1);
        }
    };
    let mut session = match probe.attach("esp32s3", Permissions::default()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[flash] attach 失败: {e}");
            std::process::exit(1);
        }
    };

    // 2) 下载固件
    eprintln!("[flash] 下载 {elf} ...");
    if let Err(e) = probe_rs::flashing::download_file_with_options(
        &mut session,
        std::path::Path::new(elf),
        ElfLoader(probe_rs::flashing::ElfOptions::default()),
        DownloadOptions::default(),
    ) {
        eprintln!("[flash] 下载失败: {e:?}");
        std::process::exit(1);
    }

    // 3) 复位运行
    let mut core = session.core(0).expect("core");
    core.reset_and_halt(Duration::from_secs(3)).expect("reset_and_halt");
    core.run().expect("run");
    eprintln!("[flash] 完成:固件已烧录并复位运行");
}
