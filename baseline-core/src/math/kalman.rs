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

