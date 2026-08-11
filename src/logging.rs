//! CSV sample logging, bucketed by sample count.
//!
//! Bucketing on the sample index rather than the wall clock keeps rows aligned
//! to exact sample boundaries: the PPK2 streams at a fixed rate but delivers
//! samples in large USB batches, so elapsed-time bucketing produces uneven rows
//! and timestamps that drift from the samples they describe.

use std::io::Write;

use anyhow::Result;

/// The PPK2 streams at a fixed 100 kSps.
pub const SAMPLE_RATE_HZ: u64 = 100_000;

/// Microseconds per sample (10 µs at 100 kSps).
pub const SAMPLE_PERIOD_US: u64 = 1_000_000 / SAMPLE_RATE_HZ;

/// Convert a requested bucket duration into a sample count.
///
/// Clamped to at least one sample, so an interval below the sample period
/// yields per-sample (raw) logging rather than an empty bucket.
pub fn samples_per_bucket(interval_us: u64) -> u64 {
    (interval_us / SAMPLE_PERIOD_US).max(1)
}

/// Accumulates samples into fixed-size buckets and writes one CSV row each.
pub struct BucketLogger<W: Write> {
    writer: W,
    per_bucket: u64,
    sum: f64,
    min: f32,
    max: f32,
    n: u64,
    /// Index of the first sample in the bucket currently being filled.
    bucket_start: u64,
}

impl<W: Write> BucketLogger<W> {
    /// Creates a logger and writes the CSV header.
    pub fn new(mut writer: W, per_bucket: u64) -> Result<Self> {
        writeln!(writer, "elapsed_us,avg_ua,min_ua,max_ua,n_samples")?;
        Ok(Self {
            writer,
            per_bucket: per_bucket.max(1),
            sum: 0.0,
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            n: 0,
            bucket_start: 0,
        })
    }

    /// Adds one sample, emitting a row once the bucket is full.
    pub fn push(&mut self, ua: f32) -> Result<()> {
        self.sum += ua as f64;
        self.min = self.min.min(ua);
        self.max = self.max.max(ua);
        self.n += 1;
        if self.n >= self.per_bucket {
            self.emit()?;
        }
        Ok(())
    }

    /// Emits any partially filled bucket and flushes the writer.
    pub fn finish(&mut self) -> Result<()> {
        if self.n > 0 {
            self.emit()?;
        }
        self.writer.flush()?;
        Ok(())
    }

    fn emit(&mut self) -> Result<()> {
        let (min, max, n) = (self.min, self.max, self.n);
        let elapsed_us = self.bucket_start * SAMPLE_PERIOD_US;
        let avg = self.sum / n as f64;
        writeln!(self.writer, "{elapsed_us},{avg:.3},{min:.3},{max:.3},{n}")?;

        self.bucket_start += n;
        self.sum = 0.0;
        self.min = f32::INFINITY;
        self.max = f32::NEG_INFINITY;
        self.n = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(buf: &[u8]) -> Vec<String> {
        String::from_utf8(buf.to_vec())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn interval_converts_to_sample_count() {
        assert_eq!(samples_per_bucket(100_000), 10_000); // 100 ms default
        assert_eq!(samples_per_bucket(1_000), 100); // 1 ms
        assert_eq!(samples_per_bucket(300), 30); // a 300 µs burst
        assert_eq!(samples_per_bucket(10), 1); // one sample
        assert_eq!(samples_per_bucket(1), 1); // below the sample period
        assert_eq!(samples_per_bucket(0), 1); // never zero
    }

    #[test]
    fn header_is_written_on_construction() {
        let mut buf = Vec::new();
        BucketLogger::new(&mut buf, 1).unwrap();
        assert_eq!(rows(&buf)[0], "elapsed_us,avg_ua,min_ua,max_ua,n_samples");
    }

    #[test]
    fn per_sample_logging_steps_by_the_sample_period() {
        let mut buf = Vec::new();
        let mut log = BucketLogger::new(&mut buf, 1).unwrap();
        for ua in [1.0f32, 2.0, 3.0] {
            log.push(ua).unwrap();
        }
        log.finish().unwrap();

        let r = rows(&buf);
        assert_eq!(r[1], "0,1.000,1.000,1.000,1");
        assert_eq!(r[2], "10,2.000,2.000,2.000,1");
        assert_eq!(r[3], "20,3.000,3.000,3.000,1");
    }

    #[test]
    fn bucket_aggregates_and_timestamps_from_sample_index() {
        let mut buf = Vec::new();
        let mut log = BucketLogger::new(&mut buf, 3).unwrap();
        // Two full buckets: a quiet one, then one containing a spike.
        for ua in [1.0f32, 2.0, 3.0, 1.0, 100.0, 1.0] {
            log.push(ua).unwrap();
        }
        log.finish().unwrap();

        let r = rows(&buf);
        assert_eq!(r[1], "0,2.000,1.000,3.000,3");
        // The spike survives in max even though avg dilutes it.
        assert_eq!(r[2], "30,34.000,1.000,100.000,3");
        assert_eq!(r.len(), 3, "no partial bucket expected");
    }

    #[test]
    fn finish_emits_a_partial_bucket() {
        let mut buf = Vec::new();
        let mut log = BucketLogger::new(&mut buf, 10).unwrap();
        log.push(5.0).unwrap();
        log.push(7.0).unwrap();
        log.finish().unwrap();

        let r = rows(&buf);
        assert_eq!(r.len(), 2, "partial bucket should be flushed");
        assert_eq!(r[1], "0,6.000,5.000,7.000,2");
    }

    #[test]
    fn a_300us_burst_is_resolved_at_10us_buckets() {
        // 30 samples of burst inside 1000 samples of idle: at the 100 ms
        // default the burst is one row's max, at 10 µs it is 30 distinct rows.
        let mut buf = Vec::new();
        let mut log = BucketLogger::new(&mut buf, 1).unwrap();
        for i in 0..1000 {
            log.push(if (500..530).contains(&i) {
                15_000.0
            } else {
                3.0
            })
            .unwrap();
        }
        log.finish().unwrap();

        let all = rows(&buf);
        let burst: Vec<&String> = all[1..]
            .iter()
            .filter(|r| r.contains("15000.000"))
            .collect();
        assert_eq!(
            burst.len(),
            30,
            "every burst sample should have its own row"
        );
        assert!(burst[0].starts_with("5000,"), "burst starts at 5000 µs");
    }
}
