/// Direct transcription of the C# `KalmanFilter` (both the one nested in
/// `MathService` and the standalone one in `Infrastructure/Services/Observation`
/// are bit-for-bit identical algorithms).
#[derive(Debug, Clone, Copy)]
pub struct KalmanFilter {
    a: f64,
    h: f64,
    q: f64,
    r: f64,
    x: f64,
    p: f64,
}

impl KalmanFilter {
    pub fn new(a: f64, h: f64, q: f64, r: f64, initial_p: f64, initial_x: f64) -> Self {
        Self {
            a,
            h,
            q,
            r,
            x: initial_x,
            p: initial_p,
        }
    }

    #[inline]
    pub fn set_r(&mut self, value: f64) {
        self.r = value;
    }

    #[inline]
    pub fn get_r(&self) -> f64 {
        self.r
    }

    #[inline]
    pub fn set_q(&mut self, value: f64) {
        self.q = value;
    }

    #[inline]
    pub fn get_q(&self) -> f64 {
        self.q
    }

    #[inline]
    pub fn output(&mut self, input: f64) -> f64 {
        // Time update
        self.x = self.a * self.x;
        self.p = self.a * self.p * self.a + self.q;
        // Measurement update
        let k = self.p * self.h / (self.h * self.p * self.h + self.r);
        self.x += k * (input - self.h * self.x);
        self.p = (1.0 - k * self.h) * self.p;
        self.x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_input_converges_to_value() {
        let mut kalman = KalmanFilter::new(1.0, 1.0, 0.01, 0.1, 1.0, 0.0);
        let mut last_output = 0.0;
        for _ in 0..100 {
            last_output = kalman.output(10.0);
        }
        assert!((last_output - 10.0).abs() < 0.5);
    }

    #[test]
    fn set_r_updates_noise_parameter() {
        let mut kalman = KalmanFilter::new(1.0, 1.0, 0.1, 1.0, 1.0, 0.0);
        kalman.set_r(5.0);
        assert_eq!(kalman.get_r(), 5.0);
    }

    #[test]
    fn set_q_updates_process_noise() {
        let mut kalman = KalmanFilter::new(1.0, 1.0, 0.1, 1.0, 1.0, 0.0);
        kalman.set_q(0.5);
        assert_eq!(kalman.get_q(), 0.5);
    }

    #[test]
    fn high_r_slower_response() {
        let mut kalman_high_r = KalmanFilter::new(1.0, 1.0, 0.01, 10.0, 1.0, 0.0);
        let mut kalman_low_r = KalmanFilter::new(1.0, 1.0, 0.01, 0.1, 1.0, 0.0);
        let output_high_r = kalman_high_r.output(10.0);
        let output_low_r = kalman_low_r.output(10.0);
        assert!(output_low_r > output_high_r);
    }

    #[test]
    fn noisy_input_smooths_output() {
        // Deterministic LCG substitute for C#'s `new Random(42)` — the exact
        // sequence doesn't need to match, only that noise stays within +/-2,
        // matching the statistical property the C# test actually asserts.
        let mut state: u64 = 42;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 33) as f64) / (u32::MAX as f64)
        };

        let mut kalman = KalmanFilter::new(1.0, 1.0, 0.1, 1.0, 1.0, 0.0);
        let mut sum_squared_error = 0.0;
        let count = 100;
        for i in 0..count {
            let true_value = 5.0;
            let noise = (next() - 0.5) * 4.0;
            let noisy_input = true_value + noise;
            let filtered = kalman.output(noisy_input);
            if i > 20 {
                let error = filtered - true_value;
                sum_squared_error += error * error;
            }
        }
        let rms_error = (sum_squared_error / (count - 20) as f64).sqrt();
        assert!(rms_error < 1.5);
    }
}
