//! Direct transcription of the pure accumulation logic in
//! `Presentation/ViewModels/Calibration/CalibrationViewModel.DataProcessing.cs`.
//! `hex_data` here is an array of 2-hex-char byte strings (as produced by
//! `split_hex_data`), so offsets are in byte units - consistent with
//! `BaselineFileService`'s char-offset 36 == byte-offset 18 for the same
//! L1/L2 field (see `io::baseline_file_service`'s offset test).

const VOLTAGE_SCALE: f64 = (5.0 / 16383.0) * 1000.0;

#[derive(Debug, Clone, Default)]
pub struct CalibrationAccumulator {
    pub l1_columns: [Vec<f64>; 16],
    pub l2_columns: [Vec<f64>; 16],
    pub l6_columns: [Vec<f64>; 16],
    pub l7_columns: [Vec<f64>; 16],
    pub l1_volt_columns: [Vec<f64>; 16],
    pub l2_volt_columns: [Vec<f64>; 16],
    pub l6_volt_columns: [Vec<f64>; 16],
    pub l7_volt_columns: [Vec<f64>; 16],
}

impl CalibrationAccumulator {
    pub fn reset(&mut self, capacity: usize) {
        for i in 0..16 {
            self.l1_columns[i] = Vec::with_capacity(capacity);
            self.l2_columns[i] = Vec::with_capacity(capacity);
            self.l6_columns[i] = Vec::with_capacity(capacity);
            self.l7_columns[i] = Vec::with_capacity(capacity);
            self.l1_volt_columns[i] = Vec::with_capacity(capacity);
            self.l2_volt_columns[i] = Vec::with_capacity(capacity);
            self.l6_volt_columns[i] = Vec::with_capacity(capacity);
            self.l7_volt_columns[i] = Vec::with_capacity(capacity);
        }
    }

    /// Matches `ProcessCalibration`. `hex_data` is byte-pair strings, not raw hex chars.
    pub fn process_calibration(&mut self, hex_data: &[String]) {
        if hex_data.len() < 18 {
            return;
        }

        for i in 0..11usize {
            let offset_l1l2 = 18 + 64 * i;
            let offset_l6l7 = 722 + 64 * i;

            if offset_l1l2 + 64 > hex_data.len() || offset_l6l7 + 64 > hex_data.len() {
                continue;
            }

            for j in 0..16usize {
                let l1_val = parse_hex_pair(hex_data, offset_l1l2 + j * 2);
                self.l1_columns[j].push(l1_val);
                self.l1_volt_columns[j].push(l1_val * VOLTAGE_SCALE);

                let l2_val = parse_hex_pair(hex_data, offset_l1l2 + 32 + j * 2);
                self.l2_columns[j].push(l2_val);
                self.l2_volt_columns[j].push(l2_val * VOLTAGE_SCALE);

                let l6_val = parse_hex_pair(hex_data, offset_l6l7 + j * 2);
                self.l6_columns[j].push(l6_val);
                self.l6_volt_columns[j].push(l6_val * VOLTAGE_SCALE);

                let l7_val = parse_hex_pair(hex_data, offset_l6l7 + 32 + j * 2);
                self.l7_columns[j].push(l7_val);
                self.l7_volt_columns[j].push(l7_val * VOLTAGE_SCALE);
            }
        }
    }

    pub fn columns(&self, layer_index: usize) -> &[Vec<f64>; 16] {
        match layer_index {
            0 => &self.l1_columns,
            1 => &self.l2_columns,
            2 => &self.l6_columns,
            3 => &self.l7_columns,
            _ => &self.l1_columns,
        }
    }

    pub fn voltage_columns(&self, layer_index: usize) -> &[Vec<f64>; 16] {
        match layer_index {
            0 => &self.l1_volt_columns,
            1 => &self.l2_volt_columns,
            2 => &self.l6_volt_columns,
            3 => &self.l7_volt_columns,
            _ => &self.l1_volt_columns,
        }
    }
}

fn parse_hex_pair(hex_data: &[String], start_index: usize) -> f64 {
    if start_index + 1 >= hex_data.len() {
        return 0.0;
    }
    let high = i32::from_str_radix(&hex_data[start_index], 16).unwrap_or(0);
    let low = i32::from_str_radix(&hex_data[start_index + 1], 16).unwrap_or(0);
    ((high << 8) + low) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_calibration_ignores_short_input() {
        let mut acc = CalibrationAccumulator::default();
        let hex_data = vec!["00".to_string(); 10];
        acc.process_calibration(&hex_data);
        assert!(acc.l1_columns[0].is_empty());
    }

    #[test]
    fn parse_hex_pair_out_of_range_returns_zero() {
        let hex_data = vec!["AB".to_string()];
        assert_eq!(parse_hex_pair(&hex_data, 5), 0.0);
    }

    #[test]
    fn process_calibration_decodes_first_channel() {
        let mut acc = CalibrationAccumulator::default();
        let mut hex_data = vec!["00".to_string(); 18 + 64 * 11];
        // offset_l1l2 for i=0 is 18; L1 channel 0 is at hex_data[18..20].
        hex_data[18] = "AB".to_string();
        hex_data[19] = "CD".to_string();
        acc.process_calibration(&hex_data);
        let expected = ((0xAB << 8) + 0xCD) as f64;
        assert_eq!(acc.l1_columns[0][0], expected);
        assert!((acc.l1_volt_columns[0][0] - expected * VOLTAGE_SCALE).abs() < 1e-9);
    }
}
