//! YAML scenario format.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Deserializer};

/// Default replay bitrate in bits per second.
pub const DEFAULT_BITRATE: u64 = 2_000_000;
/// Prefix of `capture:` values naming a recorded file under `--captures`.
pub const FILE_PREFIX: &str = "file:";

/// One scenario file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Generator profile (see [`crate::tsgen::PROFILES`]) or `file:<name>`
    /// for `<captures>/<name>.ts`.
    pub capture: String,
    /// Number of replay adapters (`-e 0-(n-1)`).
    #[serde(default = "one")]
    pub adapters: usize,
    #[serde(default = "default_bitrate")]
    pub bitrate: u64,
    /// Extra server arguments, identical for both binaries.
    #[serde(default)]
    pub args: Vec<String>,
    /// Part of the fast `cargo test` subset.
    #[serde(default)]
    pub fast: bool,
    pub steps: Vec<Step>,
    #[serde(skip)]
    pub path: PathBuf,
}

fn one() -> usize {
    1
}

fn default_bitrate() -> u64 {
    DEFAULT_BITRATE
}

/// A scenario step, written as a one-key map. Strings are templates (see [`crate::runner`]).
#[derive(Debug, Clone)]
pub enum Step {
    /// Sends an RTSP request on a named connection and records the reply.
    Rtsp(RtspStep),
    /// One HTTP request on a fresh connection.
    Http(HttpStep),
    /// Starts an HTTP stream whose TS body is captured under `name`.
    HttpStream(HttpStreamStep),
    /// Binds an RTP/RTCP client port pair under this name.
    Udp(String),
    /// Records a normalised summary of one replay period from a source.
    Capture(CaptureStep),
    /// Records whether a source stays silent for a while.
    Silence(SilenceStep),
    /// Closes a connection (RTSP or HTTP stream).
    Close(String),
    /// Waits for the given number of milliseconds.
    Sleep(u64),
}

fn value<T: DeserializeOwned, E: de::Error>(value: serde_yaml::Value) -> Result<T, E> {
    serde_yaml::from_value(value).map_err(E::custom)
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = BTreeMap::<String, serde_yaml::Value>::deserialize(deserializer)?;
        let mut entries = map.into_iter();
        let (Some((key, body)), None) = (entries.next(), entries.next()) else {
            return Err(de::Error::custom("a step is a map with exactly one key"));
        };
        Ok(match key.as_str() {
            "rtsp" => Step::Rtsp(value(body)?),
            "http" => Step::Http(value(body)?),
            "http_stream" => Step::HttpStream(value(body)?),
            "udp" => Step::Udp(value(body)?),
            "capture" => Step::Capture(value(body)?),
            "silence" => Step::Silence(value(body)?),
            "close" => Step::Close(value(body)?),
            "sleep" => Step::Sleep(value(body)?),
            other => return Err(de::Error::custom(format!("unknown step {other}"))),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RtspStep {
    pub conn: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Adds `Session:` with the id last returned on this connection.
    #[serde(default)]
    pub session: bool,
    /// Omits the automatic `CSeq` header.
    #[serde(default)]
    pub no_cseq: bool,
    /// Sends this text verbatim instead of building a request.
    pub raw: Option<String>,
    #[serde(default = "response_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyMode {
    /// Normalised body text.
    #[default]
    Full,
    /// Body with every number replaced, for live counters.
    Numbers,
    /// Body length and hash only.
    Length,
    /// No body.
    None,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpStep {
    pub path: String,
    #[serde(default = "get")]
    pub method: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: BodyMode,
    #[serde(default = "response_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpStreamStep {
    pub name: String,
    pub path: String,
    #[serde(default = "response_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureStep {
    /// UDP client name, RTSP connection (interleaved) or HTTP stream name.
    pub from: String,
    /// Data discarded before the summary starts.
    #[serde(default = "settle")]
    pub settle_ms: u64,
    #[serde(default = "capture_timeout")]
    pub timeout_ms: u64,
    /// Packet budget when the source carries no indexed ES packets.
    #[serde(default = "max_packets")]
    pub packets: usize,
    /// Summary kind; `parse` fills it in (`sets` for recorded files).
    pub mode: Option<CaptureMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Counts over one replay period of a generated capture.
    Period,
    /// Timing-independent sets over one full loop of a recorded file.
    Sets,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SilenceStep {
    pub from: String,
    pub ms: u64,
}

fn get() -> String {
    "GET".to_owned()
}

fn response_timeout() -> u64 {
    3000
}

fn settle() -> u64 {
    300
}

fn capture_timeout() -> u64 {
    5000
}

fn max_packets() -> usize {
    2000
}

impl Scenario {
    /// Parses a scenario from YAML text.
    pub fn parse(text: &str, path: &Path) -> Result<Self, String> {
        let mut scenario: Scenario =
            serde_yaml::from_str(text).map_err(|error| format!("{}: {error}", path.display()))?;
        let file = scenario.capture_file_name().is_some();
        match scenario.capture_file_name() {
            Some(name)
                if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) =>
            {
                return Err(format!("{}: bad capture file name {name}", path.display()));
            }
            Some(_) => {}
            None if crate::tsgen::generate(&scenario.capture).is_none() => {
                return Err(format!(
                    "{}: unknown capture profile {}",
                    path.display(),
                    scenario.capture
                ));
            }
            None => {}
        }
        for step in &mut scenario.steps {
            if let Step::Capture(capture) = step {
                let mode = capture.mode.get_or_insert(if file {
                    CaptureMode::Sets
                } else {
                    CaptureMode::Period
                });
                // Recorded files carry no ES index, so periods depend on timing.
                if file && *mode == CaptureMode::Period {
                    return Err(format!("{}: file captures need mode: sets", path.display()));
                }
            }
        }
        if scenario.adapters == 0 {
            return Err(format!("{}: adapters must be at least 1", path.display()));
        }
        scenario.path = path.to_owned();
        Ok(scenario)
    }

    /// Name of the recorded capture file, for `capture: file:<name>`.
    pub fn capture_file_name(&self) -> Option<&str> {
        self.capture.strip_prefix(FILE_PREFIX)
    }

    /// Loads scenario files; directories contribute their `*.yaml` files in name order.
    pub fn load(paths: &[PathBuf]) -> Result<Vec<Self>, String> {
        let mut files = Vec::new();
        for path in paths {
            if path.is_dir() {
                let entries = std::fs::read_dir(path)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                let mut found: Vec<PathBuf> = entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .filter(|file| {
                        file.extension()
                            .is_some_and(|ext| ext == "yaml" || ext == "yml")
                    })
                    .collect();
                found.sort();
                files.extend(found);
            } else {
                files.push(path.clone());
            }
        }
        files
            .iter()
            .map(|file| {
                let text = std::fs::read_to_string(file)
                    .map_err(|error| format!("{}: {error}", file.display()))?;
                Self::parse(&text, file)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steps() {
        let text = r#"
name: demo
capture: fta
steps:
  - udp: u1
  - rtsp: { conn: c, method: OPTIONS, url: "{base}/" }
  - capture: { from: u1 }
  - sleep: 10
  - close: c
"#;
        let scenario = Scenario::parse(text, Path::new("demo.yaml")).expect("parse");
        assert_eq!(scenario.steps.len(), 5);
        assert_eq!(scenario.adapters, 1);
        assert!(matches!(scenario.steps[0], Step::Udp(ref name) if name == "u1"));
        assert!(matches!(scenario.steps[3], Step::Sleep(10)));
    }

    #[test]
    fn rejects_unknown_profile() {
        let text = "name: x\ncapture: nope\nsteps: []\n";
        assert!(Scenario::parse(text, Path::new("x.yaml")).is_err());
    }

    fn capture_mode(scenario: &Scenario) -> Option<CaptureMode> {
        scenario.steps.iter().find_map(|step| match step {
            Step::Capture(capture) => capture.mode,
            _ => None,
        })
    }

    #[test]
    fn file_captures_default_to_sets_and_reject_period() {
        let parse = |text: &str| Scenario::parse(text, Path::new("f.yaml"));
        let text = "name: f\ncapture: file:fta-dmax\nsteps: [{ capture: { from: c } }]\n";
        let scenario = parse(text).expect("parse");
        assert_eq!(scenario.capture_file_name(), Some("fta-dmax"));
        assert_eq!(capture_mode(&scenario), Some(CaptureMode::Sets));
        let text = "name: g\ncapture: fta\nsteps: [{ capture: { from: c } }]\n";
        assert_eq!(
            capture_mode(&parse(text).expect("parse")),
            Some(CaptureMode::Period)
        );
        let text = "name: f\ncapture: file:x\nsteps: [{ capture: { from: c, mode: period } }]\n";
        assert!(parse(text).is_err());
        for name in ["file:", "file:../x", "file:a/b", "file:.x"] {
            assert!(
                parse(&format!("name: f\ncapture: {name}\nsteps: []\n")).is_err(),
                "{name}"
            );
        }
    }
}
