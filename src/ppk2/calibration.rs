use anyhow::Result;

const ADC_MULTIPLIER: f32 = 1.8 / 163840.0;
const SPIKE_ALPHA: f32 = 0.18;
const SPIKE_ALPHA_RANGE4: f32 = 0.06;
const SPIKE_FILTER_SAMPLES: u8 = 3;

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
            gi: [1.0; 5],
            s: [0.0; 5],
            i: [0.0; 5],
            ug: [1.0; 5],
        }
    }
}

/// Parse the metadata text emitted by the device on GET_META_DATA.
///
/// The device emits one `KEY: VALUE` pair per line
pub fn parse_modifiers(text: &str) -> Result<Modifiers> {
    let mut m = Modifiers::default();

    for line in text.lines() {
        let line = line.trim();
        if line.contains("END") {
            break;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let Ok(value) = val.trim().parse::<f32>() else {
            continue;
        };

        // Split the trailing range index off the coefficient name.
        let Some(idx) = key
            .chars()
            .last()
            .and_then(|c| c.to_digit(10))
            .filter(|d| *d < 5)
        else {
            continue;
        };
        let name = &key[..key.len() - 1];
        let idx = idx as usize;

        match name {
            "R" if value != 0.0 => m.r[idx] = value,
            "R" => {}
            "O" => m.o[idx] = value,
            "GS" => m.gs[idx] = value,
            "GI" => m.gi[idx] = value,
            "S" => m.s[idx] = value,
            "I" => m.i[idx] = value,
            "UG" => m.ug[idx] = value,
            _ => {}
        }
    }

    Ok(m)
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

/// Spike filter applied around measurement-range changes.
///
/// Two rolling averages are maintained on every sample, but a smoothed value is
/// only substituted for the output during — and for the two samples after — a
/// range change. Steady-state samples pass through raw, so genuine transients
/// are not attenuated.
pub struct SpikeFilter {
    prev_range: u8,
    after_spike: u8,
    consecutive: u8,
    /// Rolling average at α = 0.18, used for ranges 0-3.
    avg: f32,
    /// Rolling average at α = 0.06, used for range 4.
    avg4: f32,
    initialized: bool,
}

impl SpikeFilter {
    pub fn new() -> Self {
        Self {
            prev_range: 0,
            after_spike: 0,
            consecutive: 0,
            avg: 0.0,
            avg4: 0.0,
            initialized: false,
        }
    }

    /// Apply the filter to one sample. Returns the µA value to record.
    pub fn apply(&mut self, ua: f32, range: u8) -> f32 {
        // First sample seeds both averages; nothing to smooth against yet.
        if !self.initialized {
            self.avg = ua;
            self.avg4 = ua;
            self.initialized = true;
            self.prev_range = range;
            return ua;
        }

        let prev_avg = self.avg;
        let prev_avg4 = self.avg4;

        // Both averages advance on every sample, whichever one we end up using.
        self.avg = SPIKE_ALPHA * ua + (1.0 - SPIKE_ALPHA) * self.avg;
        self.avg4 = SPIKE_ALPHA_RANGE4 * ua + (1.0 - SPIKE_ALPHA_RANGE4) * self.avg4;

        let changed = range != self.prev_range;
        let out = if changed || self.after_spike > 0 {
            if changed {
                // A range change is accepted immediately — no consecutive-sample gate.
                self.consecutive = 0;
                self.after_spike = SPIKE_FILTER_SAMPLES;
            } else {
                self.consecutive += 1;
            }

            // `range.min(4)` in adc_to_microamps treats anything above 4 as 4;
            // the 3-bit range field can encode 5-7, so match that clamp here.
            let out = if range >= 4 {
                if self.consecutive < 2 {
                    // The samples straddling a switch into the high-current range
                    // are unreliable — undo their contribution to both averages.
                    self.avg = prev_avg;
                    self.avg4 = prev_avg4;
                }
                self.avg4
            } else {
                self.avg
            };

            self.after_spike -= 1;
            out
        } else {
            ua
        };

        self.prev_range = range;
        out
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

    const DEVICE_METADATA: &str = "Calibrated: 1\n\
                                   R0: 1003.3\n\
                                   R1: 100.7\n\
                                   R2: 10.15\n\
                                   R3: 0.94\n\
                                   R4: 0.043\n\
                                   GS0: 0.0\n\
                                   GS1: 0.001\n\
                                   GI0: 1.0\n\
                                   GI1: 0.998\n\
                                   O0: 5.5\n\
                                   O1: 6.5\n\
                                   S0: 0.0001\n\
                                   I0: 2.0e-8\n\
                                   UG0: 1.0\n\
                                   HW: 3\n\
                                   IA: 5\n\
                                   END\n";

    #[test]
    fn parse_modifiers_reads_indexed_colon_format() {
        let m = parse_modifiers(DEVICE_METADATA).unwrap();
        assert!(approx_eq(m.r[0], 1003.3, EPSILON), "got {}", m.r[0]);
        assert!(approx_eq(m.r[1], 100.7, EPSILON), "got {}", m.r[1]);
        assert!(approx_eq(m.r[4], 0.043, EPSILON), "got {}", m.r[4]);
        assert!(approx_eq(m.gi[1], 0.998, EPSILON), "got {}", m.gi[1]);
        assert!(approx_eq(m.o[1], 6.5, EPSILON), "got {}", m.o[1]);
        assert!(approx_eq(m.s[0], 0.0001, 1e-6), "got {}", m.s[0]);
    }

    #[test]
    fn parse_modifiers_ignores_scalar_and_unknown_keys() {
        let m = parse_modifiers(DEVICE_METADATA).unwrap();
        assert!(approx_eq(m.ug[3], 1.0, EPSILON));
        assert!(approx_eq(m.gi[4], 1.0, EPSILON));
    }

    #[test]
    fn parse_modifiers_stops_at_end() {
        let text = "R0: 1.0\nEND\nR0: 9.0\n";
        let m = parse_modifiers(text).unwrap();
        assert!(approx_eq(m.r[0], 1.0, EPSILON));
    }

    #[test]
    fn parse_modifiers_rejects_zero_shunt() {
        let m = parse_modifiers("R2: 0\nEND\n").unwrap();
        assert!(approx_eq(m.r[2], 10.15, EPSILON), "got {}", m.r[2]);
    }

    #[test]
    fn parse_modifiers_skips_malformed_lines_without_losing_others() {
        let m = parse_modifiers("R0: not_a_number\nR1: 55.5\nEND\n").unwrap();
        assert!(approx_eq(m.r[0], 1031.64, EPSILON), "default retained");
        assert!(approx_eq(m.r[1], 55.5, EPSILON), "later key still parsed");
    }

    #[test]
    fn default_gain_offset_is_identity_not_squared() {
        // gi must default to 1.0: `result * (gs * result + gi)` with gi = 0
        // collapses to result², which reads as ~0 for any real current.
        let mods = Modifiers::default();
        assert!(approx_eq(mods.gi[0], 1.0, EPSILON));

        // A mid-range ADC value should land in a plausible µA range, not ~0.
        let ua = adc_to_microamps(30_000, 0, 3300, &mods);
        assert!(ua > 100.0, "expected a real current, got {ua}");
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
    fn spike_filter_steady_state_returns_raw() {
        let mut f = SpikeFilter::new();
        assert!(approx_eq(f.apply(100.0, 0), 100.0, EPSILON));
        // No range change, so samples must pass through untouched rather than
        // being low-passed by the rolling average.
        let v1 = f.apply(200.0, 0);
        let v2 = f.apply(300.0, 0);
        assert!(
            approx_eq(v1, 200.0, EPSILON),
            "expected raw 200.0, got {v1}"
        );
        assert!(
            approx_eq(v2, 300.0, EPSILON),
            "expected raw 300.0, got {v2}"
        );
    }

    #[test]
    fn spike_filter_smooths_exactly_three_samples_after_range_change() {
        let mut f = SpikeFilter::new();
        f.apply(100.0, 0);
        f.apply(100.0, 0);

        // Switch to range 2: three smoothed samples at α=0.18, then raw.
        let v1 = f.apply(1100.0, 2);
        let v2 = f.apply(1100.0, 2);
        let v3 = f.apply(1100.0, 2);
        let v4 = f.apply(1100.0, 2);

        assert!(approx_eq(v1, 280.0, EPSILON), "expected 280.0, got {v1}");
        assert!(approx_eq(v2, 427.6, EPSILON), "expected 427.6, got {v2}");
        assert!(
            approx_eq(v3, 548.632, EPSILON),
            "expected 548.632, got {v3}"
        );
        assert!(
            approx_eq(v4, 1100.0, EPSILON),
            "spike window should be over: expected raw 1100.0, got {v4}"
        );
    }

    #[test]
    fn spike_filter_dither_does_not_suppress_high_range_samples() {
        // Regression test for the range-hold bug: alternating ranges used to
        // discard every high-range sample, so the output never left the low
        // value. The high samples must reach the output via the rolling average.
        let mut f = SpikeFilter::new();
        f.apply(10.0, 0);

        let v1 = f.apply(1010.0, 2);
        let v2 = f.apply(10.0, 0);
        let v3 = f.apply(1010.0, 2);
        assert!(approx_eq(v1, 190.0, EPSILON), "expected 190.0, got {v1}");
        assert!(approx_eq(v2, 157.6, EPSILON), "expected 157.6, got {v2}");
        assert!(
            approx_eq(v3, 311.032, EPSILON),
            "expected 311.032, got {v3}"
        );

        let mut last = v3;
        for _ in 0..5 {
            f.apply(10.0, 0);
            last = f.apply(1010.0, 2);
        }
        assert!(
            last > 100.0,
            "dithered high-range samples must reach the output, got {last}"
        );
    }

    #[test]
    fn spike_filter_rolls_back_average_entering_range4() {
        let mut f = SpikeFilter::new();
        f.apply(100.0, 0);
        f.apply(100.0, 0);

        // Switching into range 4 discards the first two samples' contribution,
        // so the output holds at the pre-transition average.
        let v1 = f.apply(5000.0, 4);
        let v2 = f.apply(5000.0, 4);
        // Third sample is kept: 0.06 * 5000 + 0.94 * 100 = 394.0
        let v3 = f.apply(5000.0, 4);
        let v4 = f.apply(5000.0, 4);

        assert!(approx_eq(v1, 100.0, EPSILON), "expected 100.0, got {v1}");
        assert!(approx_eq(v2, 100.0, EPSILON), "expected 100.0, got {v2}");
        assert!(approx_eq(v3, 394.0, EPSILON), "expected 394.0, got {v3}");
        assert!(
            approx_eq(v4, 5000.0, EPSILON),
            "spike window should be over: expected raw 5000.0, got {v4}"
        );
    }
}
