//! Excel export, covering `BaselineFileService.SaveToExcelAsync` and
//! `ObservationExcelHelper.SaveAllResultsToExcelAsync`/`SaveToExcelAsync`.
//! EPPlus (write) -> `rust_xlsxwriter`; `ExcelDataReader` (read) -> `calamine`
//! is used by `read_baseline_excel` below.

use crate::models::baseline::BaselineData;
use rust_xlsxwriter::{Workbook, Worksheet};
use std::path::Path;

const CHANNELS_PER_LAYER: u16 = 16;

/// Matches `BaselineFileService.SaveToExcelAsync`: one row per `BaselineData`,
/// columns [PacketNo, SamplingNo, L1 CH1..16, L2 CH1..16, L6 CH1..16, L7 CH1..16].
pub fn save_baseline_to_excel(data: &[BaselineData], file_path: impl AsRef<Path>) -> Result<(), String> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet().set_name("Processed Data").map_err(|e| e.to_string())?;

    write_baseline_headers(sheet)?;

    for (row, item) in data.iter().enumerate() {
        let r = (row + 1) as u32;
        sheet.write_number(r, 0, item.sampling_packet_no as f64).map_err(|e| e.to_string())?;
        sheet.write_number(r, 1, item.sampling_no as f64).map_err(|e| e.to_string())?;

        let mut c = 2u16;
        for layer in [&item.l1, &item.l2, &item.l6, &item.l7] {
            for &v in layer.iter() {
                sheet.write_number(r, c, v as f64).map_err(|e| e.to_string())?;
                c += 1;
            }
        }
    }

    workbook.save(file_path.as_ref()).map_err(|e| e.to_string())
}

fn write_baseline_headers(sheet: &mut Worksheet) -> Result<(), String> {
    sheet.write_string(0, 0, "Sampling Packet No.").map_err(|e| e.to_string())?;
    sheet.write_string(0, 1, "Sampling No.").map_err(|e| e.to_string())?;

    let mut col = 2u16;
    for layer in ["L1", "L2", "L6", "L7"] {
        for i in 1..=CHANNELS_PER_LAYER {
            sheet.write_string(0, col, format!("{layer} CH{i}")).map_err(|e| e.to_string())?;
            col += 1;
        }
    }
    Ok(())
}

/// Matches `ObservationExcelHelper.SaveToExcelAsync`: one string value per
/// row, single column.
pub fn save_lines_to_excel(lines: &[String], file_path: impl AsRef<Path>) -> Result<(), String> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet().set_name("Processed Data").map_err(|e| e.to_string())?;
    for (i, line) in lines.iter().enumerate() {
        sheet.write_string(i as u32, 0, line).map_err(|e| e.to_string())?;
    }
    workbook.save(file_path.as_ref()).map_err(|e| e.to_string())
}

/// Reads a baseline Excel workbook produced by [`save_baseline_to_excel`]
/// back into `BaselineData`, matching `BaselineFileService.ReadExcelFileAsync`.
/// Reads column A of the first sheet as strings, one entry per row (no
/// header row skipped) - matches the `ExcelDataReader` usage in
/// Calibration/Flux/Observation's `ReadData`/`ProcessExcelDataAsync`, which
/// read every row starting at row 1.
pub fn read_excel_column_a(file_path: impl AsRef<Path>) -> Result<Vec<String>, String> {
    use calamine::{open_workbook, Data, Reader, Xlsx};
    use std::io::BufReader;

    let mut workbook = open_workbook::<Xlsx<BufReader<std::fs::File>>, _>(file_path.as_ref())
        .map_err(|e| e.to_string())?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| "Excel file found but appears empty.".to_string())?;
    let range = workbook.worksheet_range(&sheet_name).map_err(|e| e.to_string())?;

    Ok(range
        .rows()
        .map(|row| match row.first() {
            Some(Data::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        })
        .collect())
}

pub fn read_baseline_excel(file_path: impl AsRef<Path>) -> Result<Vec<BaselineData>, String> {
    use calamine::{open_workbook, DataType, Reader, Xlsx};
    use std::io::BufReader;

    let mut workbook = open_workbook::<Xlsx<BufReader<std::fs::File>>, _>(file_path.as_ref())
        .map_err(|e| e.to_string())?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| "Excel file found but appears empty.".to_string())?;
    let range = workbook.worksheet_range(&sheet_name).map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    for row in range.rows().skip(1) {
        if row.is_empty() {
            continue;
        }
        let get_i32 = |c: usize| row.get(c).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let get_f32 = |c: usize| row.get(c).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;

        let mut data = BaselineData {
            sampling_packet_no: get_i32(0),
            sampling_no: get_i32(1),
            ..Default::default()
        };

        const VOLTAGE_FACTOR: f32 = (5.0 / 16383.0) * 1000.0;
        let mut c = 2usize;
        for (layer, volt) in [
            (&mut data.l1, &mut data.l1_voltage),
            (&mut data.l2, &mut data.l2_voltage),
            (&mut data.l6, &mut data.l6_voltage),
            (&mut data.l7, &mut data.l7_voltage),
        ] {
            for i in 0..16 {
                let val = get_f32(c);
                layer[i] = val;
                volt[i] = val * VOLTAGE_FACTOR;
                c += 1;
            }
        }

        results.push(data);
    }

    Ok(results)
}
