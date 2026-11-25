//! Saves each utterance as the recognizer heard it, for listening back and for
//! re-running other models on real speech (`[audio] record_utterances`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::SAMPLE_RATE;

pub struct Recorder {
    dir: PathBuf,
    model: String,
}

impl Recorder {
    /// `model` is written next to each transcript, to compare models later.
    pub fn new(dir: &Path, model: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.into(),
            model: model.into(),
        })
    }

    /// Writes `<UTC time>.wav` and appends `file, model, text` to `transcripts.tsv`.
    pub fn save(&self, samples: &[f32], text: &str) {
        let name = format!("{}.wav", utc_stamp(SystemTime::now()));
        let wav = self.dir.join(&name);
        if !sherpa_onnx::write(&wav.to_string_lossy(), samples, SAMPLE_RATE) {
            tracing::warn!("couldn't write {}", wav.display());
            return;
        }
        let line = format!(
            "{name}\t{}\t{}\n",
            self.model,
            text.replace(['\t', '\n'], " ")
        );
        let index = self.dir.join("transcripts.tsv");
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&index)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = result {
            tracing::warn!("couldn't append to {}: {e}", index.display());
        }
    }
}

/// `YYYYMMDD-HHMMSS.mmm`, UTC (local time isn't safe to look up from a thread).
fn utc_stamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{day:02}-{:02}{:02}{:02}.{:03}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stamps() {
        let at = |s| utc_stamp(UNIX_EPOCH + Duration::from_millis(s));
        assert_eq!(at(0), "19700101-000000.000");
        // 2026-11-24 18:30:05.042 UTC
        assert_eq!(at(1_795_545_005_042), "20261124-183005.042");
        assert_eq!(at(951_782_400_000), "20000229-000000.000");
    }
}
