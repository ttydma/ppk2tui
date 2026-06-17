use anyhow::Result;

const ADC_MULTIPLIER: f32 = 1.8 / 163840.0;
const SPIKE_ALPHA_NORMAL: f32 = 0.18;
const SPIKE_ALPHA_HIGH_SENS: f32 = 0.06;
const RANGE_CHANGE_HOLD: u8 = 3;

/// Per-range calibration coefficients fetched from device metadata.
#[derive(Debug, Clone)]
pub struct Modifiers {
    pub r: [f32; 5],
    pub o: [f32; 5],
    pub gs: [f32; 5],
    pub gi: [f32; 5],
    pub s: [f32; 5],
    pub i: [f32; 5],
    pub ug: [f32; 5],
}

impl Default for Modifiers {
    fn default() -> Self {
        Self {
            r: [1031.64, 101.65, 10.15, 0.94, 0.043],
            o: [0.0; 5],
            gs: [1.0; 5],
            gi: [0.0; 5],
            s: [0.0; 5],
            i: [0.0; 5],
            ug: [1.0; 5],
        }
    }
}

/// Parse the metadata text emitted by the device on GET_META_DATA.
/// Lines are `key=value` pairs; values may be comma-separated per-range lists.
/// Parsing stops at a line containing "END".
pub fn parse_modifiers(text: &str) -> Result<Modifiers> {
    let mut m = Modifiers::default();

    for line in text.lines() {
        let line = line.trim();
        if line.contains("END") {
            break;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            // Only attempt float parsing for known coefficient keys
            match key {
                "R" | "O" | "GS" | "GI" | "S" | "I" | "UG" => {
                    let arr = parse_float_array(val)?;
                    match key {
                        "R" => fill_array(&mut m.r, &arr),
                        "O" => fill_array(&mut m.o, &arr),
                        "GS" => fill_array(&mut m.gs, &arr),
                        "GI" => fill_array(&mut m.gi, &arr),
                        "S" => fill_array(&mut m.s, &arr),
                        "I" => fill_array(&mut m.i, &arr),
                        "UG" => fill_array(&mut m.ug, &arr),
                        _ => unreachable!(),
                    }
                }
                _ => {}
            }
        }
    }

    Ok(m)
}

fn parse_float_array(s: &str) -> Result<Vec<f32>> {
    s.split(',')
        .map(|v| {
            v.trim()
                .parse::<f32>()
                .map_err(|e| anyhow::anyhow!("parse float '{}': {}", v.trim(), e))
        })
        .collect()
}

fn fill_array(dest: &mut [f32; 5], src: &[f32]) {
    for (d, s) in dest.iter_mut().zip(src.iter()) {
        *d = *s;
    }
}

/// Convert a raw ADC value to microamps using calibration coefficients.
pub fn adc_to_microamps(adc: u16, range: u8, vdd_mv: u16, mods: &Modifiers) -> f32 {
    let r = range.min(4) as usize;
    let result = (adc as f32 - mods.o[r]) * ADC_MULTIPLIER / mods.r[r];
    let gs = mods.gs[r];
    let gi = mods.gi[r];
    let s = mods.s[r];
    let i_coeff = mods.i[r];
    let ug = mods.ug[r];
    let vdd = vdd_mv as f32;
    let ua = ug * (result * (gs * result + gi) + s * (vdd / 1000.0) + i_coeff) * 1_000_000.0;
    // Guard against NaN/infinity from bad calibration data (e.g. R=0 → division by zero)
    if ua.is_finite() {
        ua
    } else {
        0.0
    }
}

/// Exponential-moving-average spike filter with range-change hysteresis.
pub struct SpikeFilter {
    prev_range: u8,
    range_hold_count: u8,
    ema: f32,
    initialized: bool,
}

impl SpikeFilter {
    pub fn new() -> Self {
        Self {
            prev_range: 0,
            range_hold_count: 0,
            ema: 0.0,
            initialized: false,
        }
    }

    /// Apply the filter to one sample. Returns the smoothed µA value.
    pub fn apply(&mut self, ua: f32, range: u8) -> f32 {
        // First sample: initialize unconditionally, no hysteresis needed
        if !self.initialized {
            self.ema = ua;
            self.initialized = true;
            self.prev_range = range;
            return ua;
        }

        if range != self.prev_range {
            self.range_hold_count += 1;
            if self.range_hold_count < RANGE_CHANGE_HOLD {
                // Suppress range change until we've seen it consistently
                return self.ema;
            }
            self.prev_range = range;
            self.range_hold_count = 0;
        } else {
            self.range_hold_count = 0;
        }

        let alpha = if range >= 4 {
            SPIKE_ALPHA_HIGH_SENS
        } else {
            SPIKE_ALPHA_NORMAL
        };
        self.ema = alpha * ua + (1.0 - alpha) * self.ema;
        self.ema
    }
}

impl Default for SpikeFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-3;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn parse_modifiers_basic() {
        let text = "R=100.0,200.0,300.0,400.0,500.0\n\
                    O=1.0,2.0,3.0,4.0,5.0\n\
                    GS=0.1,0.2,0.3,0.4,0.5\n\
                    GI=0.01,0.02,0.03,0.04,0.05\n\
                    S=0.0,0.0,0.0,0.0,0.0\n\
                    I=0.0,0.0,0.0,0.0,0.0\n\
                    UG=1.0,1.0,1.0,1.0,1.0\n\
                    END\n";
        let m = parse_modifiers(text).unwrap();
        assert!(approx_eq(m.r[0], 100.0, EPSILON));
        assert!(approx_eq(m.r[4], 500.0, EPSILON));
        assert!(approx_eq(m.o[2], 3.0, EPSILON));
        assert!(approx_eq(m.gs[1], 0.2, EPSILON));
    }

    #[test]
    fn parse_modifiers_stops_at_end() {
        let text = "R=1.0,2.0,3.0,4.0,5.0\nEND\nR=9.0,9.0,9.0,9.0,9.0\n";
        let m = parse_modifiers(text).unwrap();
        assert!(approx_eq(m.r[0], 1.0, EPSILON));
    }

    #[test]
    fn parse_modifiers_ignores_unknown_keys() {
        let text = "HW=1\nCALIBRATED=true\nR=1.0,2.0,3.0,4.0,5.0\nEND\n";
        let m = parse_modifiers(text).unwrap();
        assert!(approx_eq(m.r[0], 1.0, EPSILON));
    }

    #[test]
    fn adc_to_microamps_zero_adc_default_mods() {
        let mods = Modifiers::default();
        // With default mods: o[0]=0, gs[0]=1, gi[0]=0, s[0]=0, i[0]=0, ug[0]=1
        // result = 0 * ADC_MULT / R[0]; tiny value; * 1e6 should be near 0
        let ua = adc_to_microamps(0, 0, 3300, &mods);
        assert!(ua.abs() < 1.0, "expected near zero, got {ua}");
    }

    #[test]
    fn adc_to_microamps_nonzero() {
        let mods = Modifiers::default();
        let ua = adc_to_microamps(8192, 0, 3300, &mods);
        // Should produce a positive value
        assert!(ua > 0.0, "expected positive µA, got {ua}");
    }

    #[test]
    fn spike_filter_range_change_hysteresis() {
        let mut f = SpikeFilter::new();
        // Seed with range 0
        f.apply(100.0, 0);
        let v0 = f.apply(100.0, 0);
        // Switch to range 1 — first two samples should return old EMA, not new value
        let v1 = f.apply(1000.0, 1);
        let v2 = f.apply(1000.0, 1);
        // Third same-range sample should accept the change
        let v3 = f.apply(1000.0, 1);
        assert!(
            approx_eq(v1, v0, EPSILON),
            "range change should be suppressed on 1st sample"
        );
        assert!(
            approx_eq(v2, v0, EPSILON),
            "range change should be suppressed on 2nd sample"
        );
        assert!(v3 > v0, "range change should be accepted on 3rd sample");
    }

    #[test]
    fn spike_filter_ema_alpha_differs_for_range4() {
        // Range 4 uses α=0.06, range 0 uses α=0.18
        // After one step from 0 to 100: EMA should be 6.0 for range 4, 18.0 for range 0
        let mut f4 = SpikeFilter::new();
        f4.apply(0.0, 4); // initialize
        let v4 = f4.apply(100.0, 4);
        assert!(
            approx_eq(v4, 6.0, 0.01),
            "range 4 alpha=0.06: expected 6.0, got {v4}"
        );

        let mut f0 = SpikeFilter::new();
        f0.apply(0.0, 0); // initialize
        let v0 = f0.apply(100.0, 0);
        assert!(
            approx_eq(v0, 18.0, 0.01),
            "range 0 alpha=0.18: expected 18.0, got {v0}"
        );
    }
}
