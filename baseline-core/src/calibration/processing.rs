//! Accumulation logic for Calibration mode's per-channel histograms.
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

    pub fn process_calibration(&mut self, hex_data: &[String]) {
        if hex_data.len() < 18 {
            return;
        }

        for i in 0..11usize {
            let offset = 18 + 128 * i;

            if offset + 128 > hex_data.len() {
                continue;
            }

            for j in 0..16usize {
                let l1_val = parse_hex_pair(hex_data, offset + j * 2);
                self.l1_columns[j].push(l1_val);
                self.l1_volt_columns[j].push(l1_val * VOLTAGE_SCALE);

                let l2_val = parse_hex_pair(hex_data, offset + 32 + j * 2);
                self.l2_columns[j].push(l2_val);
                self.l2_volt_columns[j].push(l2_val * VOLTAGE_SCALE);

                let l6_val = parse_hex_pair(hex_data, offset + 64 + j * 2);
                self.l6_columns[j].push(l6_val);
                self.l6_volt_columns[j].push(l6_val * VOLTAGE_SCALE);

                let l7_val = parse_hex_pair(hex_data, offset + 96 + j * 2);
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

