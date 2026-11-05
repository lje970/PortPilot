// PortPilot · 注册表视角诊断:用 nusb 同款 cfgmgr32 API(CM_Open_DevNode_Key
// CM_REGISTRY_HARDWARE)逐步复现 find_device_interface_path 的查询路径,
// 打印父节点/子节点/硬件键全部值,解释"PowerShell 能看到 GUID,nusb 却 os error 2"。
#![allow(non_snake_case)]

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Child, CM_Get_Device_IDW, CM_Get_DevNode_Registry_PropertyW, CM_Get_Sibling,
    CM_Locate_DevNodeW, CM_Open_DevNode_Key, CM_REGISTRY_HARDWARE, RegDisposition_OpenExisting,
};
use windows_sys::Win32::System::Registry::{RegCloseKey, RegEnumValueW, RegQueryValueExW, HKEY};

fn devinst_id(devinst: u32) -> String {
    let mut buf = [0u16; 512];
    let cr = unsafe { CM_Get_Device_IDW(devinst, buf.as_mut_ptr(), buf.len() as u32, 0) };
    if cr == 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    } else {
        format!("<cr={cr:#x}>")
    }
}

fn devinst_service(devinst: u32) -> String {
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            devinst,
            2, // CRP_DRIVER
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
            0,
        )
    };
    if cr == 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&buf[..end])
    } else {
        format!("<cr={cr:#x}>")
    }
}

fn dump_devnode(devinst: u32, tag: &str) {
    let id = devinst_id(devinst);
    eprintln!();
    eprintln!("--- {tag} devinst={devinst} id={id}");
    eprintln!("    Service = {}", devinst_service(devinst));

    let mut hkey: HKEY = 0isize;
    let cr = unsafe {
        CM_Open_DevNode_Key(
            devinst,
            0x20019, // KEY_READ
            0,
            RegDisposition_OpenExisting,
            &mut hkey,
            CM_REGISTRY_HARDWARE,
        )
    };
    if cr != 0 {
        eprintln!("    CM_Open_DevNode_Key(HARDWARE) 失败 cr={cr:#x}");
        return;
    }
    eprintln!("    硬件键(Enum 实例根)全部值:");
    let mut index = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut name_len = 256u32;
        let mut ty = 0u32;
        let mut size = 0u32;
        let cr = unsafe {
            RegEnumValueW(
                hkey,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null_mut(),
                &mut ty,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if cr != 0 {
            break;
        }
        let nend = name.iter().position(|&c| c == 0).unwrap_or(0);
        let name_s = String::from_utf16_lossy(&name[..nend]);
        if ty == 1 || ty == 7 {
            let mut buf = vec![0u8; size as usize];
            let mut sz = size;
            let cr2 = unsafe {
                RegQueryValueExW(
                    hkey,
                    name.as_mut_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    buf.as_mut_ptr(),
                    &mut sz,
                )
            };
            if cr2 == 0 {
                let u16s: Vec<u16> = buf
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let s = String::from_utf16_lossy(&u16s);
                eprintln!("      {name_s} (type={ty}) = {s:?}");
                index += 1;
                continue;
            }
        }
        eprintln!("      {name_s} (type={ty}, size={size})");
        index += 1;
    }
    unsafe { RegCloseKey(hkey) };
}

fn main() {
    let parent_id = "USB\\VID_303A&PID_1001\\E0:72:A1:D2:9B:88";
    let mut wparent: Vec<u16> = parent_id.encode_utf16().collect();
    wparent.push(0);
    let mut parent = 0u32;
    let cr = unsafe { CM_Locate_DevNodeW(&mut parent, wparent.as_ptr(), 0) };
    if cr != 0 {
        eprintln!("[diag] CM_Locate_DevNodeW 失败 cr={cr:#x}");
        return;
    }
    dump_devnode(parent, "父(复合)");

    let mut child = 0u32;
    let cr = unsafe { CM_Get_Child(&mut child, parent, 0) };
    if cr != 0 {
        eprintln!("[diag] CM_Get_Child 失败 cr={cr:#x}");
        return;
    }
    loop {
        dump_devnode(child, "子");
        let mut sib = 0u32;
        let cr = unsafe { CM_Get_Sibling(&mut sib, child, 0) };
        if cr != 0 {
            break;
        }
        child = sib;
    }
}
