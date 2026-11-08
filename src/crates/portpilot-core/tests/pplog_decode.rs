// PortPilot · 集成测试:pplog 记录 → FrameDecoder 离线重解析管线
// (decode_pplog 命令的内核路径;用雷达 30B 帧 AA FF 03 00 … 55 CC)
use portpilot_core::decoder::{DecoderConfig, FrameDecoder};
use portpilot_core::pplog::{PplogMeta, PplogWriter, DIR_RX};

#[test]
fn pplog_records_feed_decoder_offline() {
    let path = std::env::temp_dir().join(format!("pp_it_{}.pplog", std::process::id()));
    let _ = std::fs::remove_file(&path);

    // 1) 写 10 条雷达 30B 帧(ms 间隔 45)
    let mut frame = vec![0xAA, 0xFF, 0x03, 0x00];
    frame.extend_from_slice(&[0x21, 0x80, 0xAE, 0x81, 0x00, 0x00, 0x68, 0x01]);
    frame.extend_from_slice(&[0u8; 16]);
    frame.extend_from_slice(&[0x55, 0xCC]);
    assert_eq!(frame.len(), 30);

    let meta = PplogMeta {
        kind: "serial".into(),
        port: "COM10".into(),
        baud: 256000,
        encoding: "utf8".into(),
        created_utc: "2026-11-08T00:00:00Z".into(),
    };
    {
        let mut w = PplogWriter::create(&path, &meta).unwrap();
        for i in 0..10 {
            w.write_record_at(i * 45, DIR_RX, &frame).unwrap();
        }
        w.flush().unwrap();
    }

    // 2) 离线重解析:逐记录喂解码器(与 decode_pplog 命令同一路径)
    let cfg = DecoderConfig {
        name: "radar".into(),
        header: vec![0xAA, 0xFF, 0x03, 0x00],
        frame_len: None,
        tail: vec![0x55, 0xCC],
        fields: vec![],
        checksum: "none".into(),
    };
    let mut dec = FrameDecoder::new(cfg);
    let (mut rdr, m) = portpilot_core::pplog::PplogReader::open(&path).unwrap();
    assert_eq!(m.baud, 256000);

    let mut frames = 0;
    let mut ms_seen: Vec<u64> = Vec::new();
    loop {
        match rdr.next_record().unwrap() {
            Some(rec) => {
                if rec.dir != DIR_RX {
                    continue;
                }
                ms_seen.push(rec.ms);
                for f in dec.feed(&rec.data) {
                    assert!(f.ok);
                    frames += 1;
                }
            }
            None => break,
        }
    }
    assert_eq!(frames, 10, "10 条记录应解出 10 帧");
    assert_eq!(ms_seen, (0..10).map(|i| i * 45).collect::<Vec<_>>(), "ms 保序");
    let _ = std::fs::remove_file(&path);
}
