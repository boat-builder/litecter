use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// How often a URL is checked.
///
/// Weekly is the default for new URLs (set in the `urls` table DDL, the CLI's
/// `--every` default, and the app's add bar — change all three together).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Schedule {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

impl Schedule {
    pub fn interval_secs(self) -> i64 {
        match self {
            Schedule::Hourly => 3_600,
            Schedule::Daily => 86_400,
            Schedule::Weekly => 604_800,
            Schedule::Monthly => 2_592_000, // 30 days
        }
    }

    /// Next check time with ±5% jitter so large sets drift apart instead of
    /// bunching into one due-batch.
    pub fn next_from(self, now: i64) -> i64 {
        let interval = self.interval_secs();
        let jitter_span = interval / 20;
        now + interval + fastrand::i64(-jitter_span..=jitter_span)
    }
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Schedule::Hourly => "hourly",
            Schedule::Daily => "daily",
            Schedule::Weekly => "weekly",
            Schedule::Monthly => "monthly",
        })
    }
}

impl FromStr for Schedule {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "hourly" => Ok(Schedule::Hourly),
            "daily" => Ok(Schedule::Daily),
            "weekly" => Ok(Schedule::Weekly),
            "monthly" => Ok(Schedule::Monthly),
            other => anyhow::bail!("unknown schedule '{other}' (expected hourly|daily|weekly|monthly)"),
        }
    }
}
