use baseline_core::io::excel::{read_baseline_excel, save_baseline_to_excel};
use baseline_core::models::baseline::BaselineData;

#[test]
fn roundtrip_baseline_excel() {
    let mut data = Vec::new();
    for i in 0..5 {
        let mut d = BaselineData::default();
        d.sampling_packet_no = i;
        d.sampling_no = i * 2;
        for ch in 0..16 {
            d.l1[ch] = (i * 100 + ch as i32) as f32;
            d.l2[ch] = (i * 200 + ch as i32) as f32;
            d.l6[ch] = (i * 300 + ch as i32) as f32;
            d.l7[ch] = (i * 400 + ch as i32) as f32;
        }
        data.push(d);
    }

    let path = std::env::temp_dir().join("baseline_excel_roundtrip_test.xlsx");
    save_baseline_to_excel(&data, &path).expect("save failed");

    let read_back = read_baseline_excel(&path).expect("read failed");
    std::fs::remove_file(&path).ok();

    assert_eq!(read_back.len(), data.len(), "row count mismatch");
    for (orig, got) in data.iter().zip(read_back.iter()) {
        assert_eq!(orig.sampling_packet_no, got.sampling_packet_no);
        assert_eq!(orig.sampling_no, got.sampling_no);
        assert_eq!(orig.l1, got.l1);
        assert_eq!(orig.l2, got.l2);
        assert_eq!(orig.l6, got.l6);
        assert_eq!(orig.l7, got.l7);
    }
}
