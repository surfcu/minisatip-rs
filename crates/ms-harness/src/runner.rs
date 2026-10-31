//! Runs scenarios against binaries and compares normalised transcripts.
//!
//! Templates in step strings: `{base}`, `{host}`, `{rtsp}`, `{http}`,
//! `{rtp:U}`, `{rtcp:U}`, `{session:C}` and `{stream:C}`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::analysis::{self, Seams, Units};
use crate::client::{self, HttpStream, RtspConn, Shared, UdpPair};
use crate::diff::diff;
use crate::fakeoscam::{FakeOscam, Log};
use crate::normalize::{self, Normalizer};
use crate::scenario::{
    BodyMode, CaptureMode, CaptureStep, HttpStep, HttpStreamStep, LogStep, RtspStep, Scenario, Step,
};
use crate::server::{Binary, Launch, RTP_RANGE, Server};

/// Extra time a `sets` capture runs beyond one loop of the file.
const LOOP_MARGIN_MS: u64 = 1000;

/// Paths shared by every run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub html: PathBuf,
    pub workroot: PathBuf,
    /// Directory of recorded `file:` captures.
    pub captures: Option<PathBuf>,
}

/// Loop length and seams of the replayed file, for `sets` captures.
#[derive(Default)]
struct Replay {
    loop_ms: u64,
    seams: Seams,
}

struct Context<'a> {
    server: &'a Server,
    replay: Replay,
    norm: Normalizer,
    conns: HashMap<String, RtspConn>,
    udp: HashMap<String, UdpPair>,
    streams: HashMap<String, HttpStream>,
    /// Session and stream ids of closed RTSP connections.
    closed: HashMap<String, (Option<String>, Option<String>)>,
    /// The fake OSCam log and how many of its lines are already recorded.
    dvbapi: Option<(Log, usize)>,
    out: Vec<String>,
}

/// Writes the capture for `profile` into `dir` and returns its path.
pub fn capture_file(dir: &Path, profile: &str) -> Result<PathBuf, String> {
    let data =
        crate::tsgen::generate(profile).ok_or_else(|| format!("unknown profile {profile}"))?;
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let path = dir.join(format!("{profile}.ts"));
    if fs::read(&path).ok().as_deref() != Some(&data[..]) {
        fs::write(&path, &data).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(path)
}

/// Path of the capture `scenario` replays: generated, or recorded under `--captures`.
pub fn capture_path(scenario: &Scenario, options: &RunOptions) -> Result<PathBuf, String> {
    let Some(name) = scenario.capture_file_name() else {
        return capture_file(&options.workroot, &scenario.capture);
    };
    let dir = options
        .captures
        .as_ref()
        .ok_or_else(|| format!("{}: capture file:{name} needs --captures", scenario.name))?;
    let path = dir.join(format!("{name}.ts"));
    if !path.is_file() {
        return Err(format!("{}: capture not found", path.display()));
    }
    Ok(path)
}

fn replay(scenario: &Scenario, capture: &Path) -> Result<Replay, String> {
    let sets = scenario.steps.iter().any(
        |step| matches!(step, Step::Capture(capture) if capture.mode == Some(CaptureMode::Sets)),
    );
    if !sets {
        return Ok(Replay::default());
    }
    let file = fs::read(capture).map_err(|error| format!("{}: {error}", capture.display()))?;
    let bits = file.len() as u64 * 8;
    Ok(Replay {
        loop_ms: bits.saturating_mul(1000) / scenario.bitrate.max(1),
        seams: Seams::of(&file),
    })
}

/// Runs `scenario` once against `binary` and returns the normalised transcript.
pub fn run_once(
    scenario: &Scenario,
    binary: &Binary,
    options: &RunOptions,
) -> Result<Vec<String>, String> {
    let capture = capture_path(scenario, options)?;
    let replay = replay(scenario, &capture)?;
    let workdir = options.workroot.join(&scenario.name).join(&binary.label);
    fs::create_dir_all(&workdir).map_err(|error| error.to_string())?;
    let oscam = if scenario.oscam {
        Some(FakeOscam::start().map_err(|error| format!("fake oscam: {error}"))?)
    } else {
        None
    };
    let mut args = scenario.args.clone();
    if let Some(oscam) = &oscam {
        args.extend(["-o".to_owned(), format!("127.0.0.1:{}", oscam.port)]);
    }
    let launch = Launch {
        capture: &capture,
        adapters: scenario.adapters,
        bitrate: scenario.bitrate,
        args: &args,
        html: &options.html,
        workdir: &workdir,
    };
    let server = Server::start(binary, &launch).map_err(|error| error.to_string())?;
    let mut norm = Normalizer::default();
    let cache = workdir.join(format!("{}-cache", binary.label));
    norm.add_text(&cache.display().to_string(), "<CACHE>");
    norm.add_text(&workdir.display().to_string(), "<WORKDIR>");
    norm.add_text(&binary.path.display().to_string(), "<BINARY>");
    norm.add_text(&options.html.display().to_string(), "<HTML>");
    norm.add_port(server.rtsp_port, "RTSP")?;
    norm.add_port(server.http_port, "HTTP")?;
    for offset in 0..RTP_RANGE {
        norm.add_port(server.start_rtp + offset, &format!("SRTP+{offset}"))?;
    }
    if let Some(oscam) = &oscam {
        norm.add_port(oscam.port, "OSCAM")?;
    }
    let mut context = Context {
        server: &server,
        replay,
        norm,
        conns: HashMap::new(),
        udp: HashMap::new(),
        streams: HashMap::new(),
        closed: HashMap::new(),
        dvbapi: oscam.map(|oscam| (oscam.log, 0)),
        out: vec![format!("# scenario {}", scenario.name)],
    };
    for (number, step) in scenario.steps.iter().enumerate() {
        if let Err(error) = context.step(number + 1, step) {
            return Err(format!(
                "{} step {}: {error} (log: {})",
                binary.label,
                number + 1,
                server.log.display()
            ));
        }
    }
    let out = std::mem::take(&mut context.out);
    drop(context);
    Ok(out)
}

impl Context<'_> {
    fn step(&mut self, number: usize, step: &Step) -> Result<(), String> {
        match step {
            Step::Rtsp(rtsp) => self.rtsp(number, rtsp),
            Step::Http(http) => {
                self.http(number, http);
                Ok(())
            }
            Step::HttpStream(stream) => self.http_stream(number, stream),
            Step::Udp(name) => {
                // Client ports must not reuse a number that already has a placeholder.
                let pair = UdpPair::bind(&self.norm.ports()).map_err(|error| error.to_string())?;
                self.norm.add_port(pair.rtp_port, &format!("{name}.rtp"))?;
                self.norm
                    .add_port(pair.rtp_port + 1, &format!("{name}.rtcp"))?;
                self.udp.insert(name.clone(), pair);
                Ok(())
            }
            Step::Capture(capture) => self.capture(number, capture),
            Step::Silence(silence) => {
                let buffer = self.source(&silence.from)?;
                thread::sleep(Duration::from_millis(200));
                client::clear(&buffer);
                thread::sleep(Duration::from_millis(silence.ms));
                let units = client::with(&buffer, |b| b.datagrams.len() + b.bytes.len());
                self.out
                    .push(format!("== {number} silence {}", silence.from));
                self.out
                    .push(if units == 0 { "silent" } else { "not silent" }.to_owned());
                Ok(())
            }
            Step::Close(name) => {
                if let Some(mut conn) = self.conns.remove(name) {
                    conn.close();
                    let ids = (conn.session.clone(), conn.stream_id.clone());
                    self.closed.insert(name.clone(), ids);
                } else if let Some(mut stream) = self.streams.remove(name) {
                    stream.close();
                } else if self.udp.remove(name).is_none() {
                    return Err(format!("close: unknown connection {name}"));
                }
                Ok(())
            }
            Step::Sleep(ms) => {
                thread::sleep(Duration::from_millis(*ms));
                Ok(())
            }
            Step::Dvbapi(wait) => self.dvbapi(number, wait),
        }
    }

    /// Records the DVBAPI lines since the last step, up to the first one containing
    /// `until`; later lines wait for the next step, so their timing stays out of the diff.
    fn dvbapi(&mut self, number: usize, step: &LogStep) -> Result<(), String> {
        let (log, cursor) = self.dvbapi.as_mut().ok_or("no fake oscam")?;
        let deadline = Instant::now() + Duration::from_millis(step.timeout_ms);
        let end = loop {
            let found = {
                let lines = log.lock().unwrap_or_else(|e| e.into_inner());
                lines[*cursor..]
                    .iter()
                    .position(|line| line.contains(&step.until))
                    .map(|offset| {
                        (
                            *cursor + offset + 1,
                            lines[*cursor..*cursor + offset + 1].to_vec(),
                        )
                    })
            };
            if let Some(found) = found {
                break found;
            }
            if Instant::now() >= deadline {
                return Err(format!("dvbapi: no line with {:?}", step.until));
            }
            thread::sleep(Duration::from_millis(20));
        };
        *cursor = end.0;
        self.out
            .push(format!("== {number} dvbapi until {}", step.until));
        self.out.extend(end.1);
        Ok(())
    }

    fn expand(&self, template: &str) -> String {
        let mut out = String::new();
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let Some(length) = rest[start..].find('}') else {
                out.push_str(&rest[start..]);
                return out;
            };
            let token = &rest[start + 1..start + length];
            match self.token(token) {
                Some(value) => out.push_str(&value),
                None => out.push_str(&rest[start..=start + length]),
            }
            rest = &rest[start + length + 1..];
        }
        out.push_str(rest);
        out
    }

    fn token(&self, token: &str) -> Option<String> {
        let (kind, name) = token.split_once(':').unwrap_or((token, ""));
        match kind {
            "base" => Some(format!("rtsp://127.0.0.1:{}", self.server.rtsp_port)),
            "host" => Some("127.0.0.1".to_owned()),
            "rtsp" => Some(self.server.rtsp_port.to_string()),
            "http" => Some(self.server.http_port.to_string()),
            "rtp" => self.udp.get(name).map(|pair| pair.rtp_port.to_string()),
            "rtcp" => self
                .udp
                .get(name)
                .map(|pair| (pair.rtp_port + 1).to_string()),
            "session" => match self.conns.get(name) {
                Some(conn) => conn.session.clone(),
                None => self.closed.get(name).and_then(|ids| ids.0.clone()),
            },
            "stream" => match self.conns.get(name) {
                Some(conn) => conn.stream_id.clone(),
                None => self.closed.get(name).and_then(|ids| ids.1.clone()),
            },
            _ => None,
        }
    }

    fn rtsp(&mut self, number: usize, step: &RtspStep) -> Result<(), String> {
        if !self.conns.contains_key(&step.conn) {
            let conn =
                RtspConn::connect(self.server.rtsp_port).map_err(|error| error.to_string())?;
            self.conns.insert(step.conn.clone(), conn);
        }
        let text = match &step.raw {
            Some(raw) => self.expand(raw),
            None => {
                let url = self.expand(&step.url);
                let headers: Vec<(String, String)> = step
                    .headers
                    .iter()
                    .map(|(key, value)| (key.clone(), self.expand(value)))
                    .collect();
                let conn = self.conns.get_mut(&step.conn).ok_or("connection")?;
                let mut text = format!("{} {url} RTSP/1.0\r\n", step.method);
                if !step.no_cseq {
                    conn.cseq += 1;
                    text.push_str(&format!("CSeq: {}\r\n", conn.cseq));
                }
                if step.session
                    && let Some(id) = &conn.session
                {
                    text.push_str(&format!("Session: {id}\r\n"));
                }
                for (key, value) in headers {
                    text.push_str(&format!("{key}: {value}\r\n"));
                }
                text.push_str("\r\n");
                text
            }
        };
        let first = text.lines().next().unwrap_or("").to_owned();
        let title = format!(
            "== {number} rtsp {} > {}",
            step.conn,
            self.norm.text(&first)
        );
        self.out.push(title);
        let conn = self.conns.get_mut(&step.conn).ok_or("connection")?;
        let reply = conn.exchange(text.as_bytes(), Duration::from_millis(step.timeout_ms));
        match reply {
            Ok(message) => {
                let (head, body) = client::split(&message);
                for line in head.lines() {
                    if let Some(value) = normalize::header(line, "session") {
                        conn.session =
                            Some(value.split(';').next().unwrap_or("").trim().to_owned());
                    }
                    if let Some(value) = normalize::header(line, "com.ses.streamID") {
                        conn.stream_id = Some(value.to_owned());
                    }
                }
                self.record_message(&head, &body, &BodyMode::Full);
            }
            Err(marker) => self.out.push(marker),
        }
        Ok(())
    }

    fn record_message(&mut self, head: &str, body: &[u8], mode: &BodyMode) {
        let head = self.norm.text(&head.replace('\r', ""));
        if body.is_empty() || matches!(mode, BodyMode::None) {
            self.out.extend(head.lines().map(str::to_owned));
            return;
        }
        let text = String::from_utf8(body.to_vec());
        let rendered: Option<Vec<String>> = match (mode, text) {
            (BodyMode::Full, Ok(text)) => {
                let text = self.norm.text(&text.replace('\r', ""));
                Some(text.lines().map(str::to_owned).collect())
            }
            (BodyMode::Numbers, Ok(text)) => {
                let text = normalize::all_numbers(&self.norm.text(&text.replace('\r', "")));
                Some(text.lines().map(str::to_owned).collect())
            }
            _ => None,
        };
        match rendered {
            Some(lines) => {
                // Normalised bodies change length, so recompute the echoed header.
                let length = lines.join("\n").len();
                for line in head.lines() {
                    if normalize::header(line, "content-length").is_some() {
                        self.out.push(format!("Content-Length: {length}"));
                    } else {
                        self.out.push(line.to_owned());
                    }
                }
                self.out.push("--".to_owned());
                self.out.extend(lines);
            }
            None => {
                self.out.extend(head.lines().map(str::to_owned));
                self.out.push("--".to_owned());
                let hash = body.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
                    (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
                });
                self.out
                    .push(format!("<{} bytes, fnv {hash:016x}>", body.len()));
            }
        }
    }

    fn http_text(
        &self,
        method: &str,
        path: &str,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> String {
        let mut text = format!(
            "{method} {} HTTP/1.0\r\nHost: 127.0.0.1\r\n",
            self.expand(path)
        );
        for (key, value) in headers {
            text.push_str(&format!("{key}: {}\r\n", self.expand(value)));
        }
        text.push_str("\r\n");
        text
    }

    fn http(&mut self, number: usize, step: &HttpStep) {
        let text = self.http_text(&step.method, &step.path, &step.headers);
        let first = text.lines().next().unwrap_or("").to_owned();
        let title = format!("== {number} http > {}", self.norm.text(&first));
        self.out.push(title);
        match client::http_request(
            self.server.http_port,
            text.as_bytes(),
            Duration::from_millis(step.timeout_ms),
        ) {
            Ok(message) => {
                let (head, body) = client::split(&message);
                self.record_message(&head, &body, &step.body);
            }
            Err(marker) => self.out.push(marker),
        }
    }

    fn http_stream(&mut self, number: usize, step: &HttpStreamStep) -> Result<(), String> {
        let text = self.http_text("GET", &step.path, &Default::default());
        let first = text.lines().next().unwrap_or("").to_owned();
        let title = format!(
            "== {number} http-stream {} > {}",
            step.name,
            self.norm.text(&first)
        );
        self.out.push(title);
        let (stream, head) = HttpStream::open(
            self.server.http_port,
            text.as_bytes(),
            Duration::from_millis(step.timeout_ms),
        )?;
        self.record_message(&head, &[], &BodyMode::None);
        self.streams.insert(step.name.clone(), stream);
        Ok(())
    }

    fn source(&self, name: &str) -> Result<Shared, String> {
        if let Some(pair) = self.udp.get(name) {
            return Ok(pair.rtp.clone());
        }
        if let Some(conn) = self.conns.get(name) {
            return Ok(conn.interleaved.clone());
        }
        if let Some(stream) = self.streams.get(name) {
            return Ok(stream.data.clone());
        }
        Err(format!("unknown source {name}"))
    }

    fn capture(&mut self, number: usize, step: &CaptureStep) -> Result<(), String> {
        let buffer = self.source(&step.from)?;
        let is_ts = self.streams.contains_key(&step.from);
        thread::sleep(Duration::from_millis(step.settle_ms));
        client::clear(&buffer);
        if step.mode == Some(CaptureMode::Sets) {
            // One whole loop, so every PID and section of the file is seen.
            thread::sleep(Duration::from_millis(self.replay.loop_ms + LOOP_MARGIN_MS));
            let lines = client::with(&buffer, |b| {
                let view = if is_ts {
                    Units::Ts(&b.bytes)
                } else {
                    Units::Rtp(&b.datagrams)
                };
                analysis::summarize_sets(&view, &self.replay.seams)
            });
            // Two silent servers would otherwise produce matching transcripts.
            if lines.iter().any(|line| line.contains("packets=none")) {
                return Err(format!("capture {}: no TS packets received", step.from));
            }
            self.out.push(format!("== {number} capture {}", step.from));
            self.out.extend(lines);
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_millis(step.timeout_ms);
        let units = |b: &client::Buffer| -> Vec<String> {
            let view = if is_ts {
                Units::Ts(&b.bytes)
            } else {
                Units::Rtp(&b.datagrams)
            };
            analysis::summarize(&view)
        };
        loop {
            let done = client::with(&buffer, |b| {
                let view = if is_ts {
                    Units::Ts(&b.bytes)
                } else {
                    Units::Rtp(&b.datagrams)
                };
                analysis::complete(&view, step.packets)
            });
            if done || Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let lines = client::with(&buffer, units);
        self.out.push(format!("== {number} capture {}", step.from));
        self.out.extend(lines);
        Ok(())
    }
}

/// Result of comparing one scenario.
pub struct Outcome {
    pub name: String,
    pub passed: bool,
    pub report: String,
}

/// What a comparison checks.
pub struct Comparison<'a> {
    pub a: &'a Binary,
    pub b: &'a Binary,
    pub runs: usize,
    pub golden: Option<&'a Path>,
    pub record: Option<&'a Path>,
}

/// Runs A and B in parallel `runs` times; any A/B, run-to-run or golden diff fails.
pub fn compare(scenario: &Scenario, comparison: &Comparison<'_>, options: &RunOptions) -> Outcome {
    let mut report = String::new();
    let mut passed = true;
    let mut first: Option<(Vec<String>, Vec<String>)> = None;
    let golden = comparison.golden.and_then(|dir| {
        fs::read_to_string(dir.join(format!("{}.txt", scenario.name)))
            .ok()
            .map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
    });
    if comparison.golden.is_some() && golden.is_none() {
        passed = false;
        report.push_str("missing golden transcript\n");
    }
    for run in 0..comparison.runs.max(1) {
        let a = Binary {
            label: format!("A-{}", comparison.a.label),
            path: comparison.a.path.clone(),
        };
        let b = Binary {
            label: format!("B-{}", comparison.b.label),
            path: comparison.b.path.clone(),
        };
        let (ta, tb) = thread::scope(|scope| {
            let ha = scope.spawn(|| run_once(scenario, &a, options));
            let hb = scope.spawn(|| run_once(scenario, &b, options));
            (
                ha.join().unwrap_or_else(|_| Err("panic".to_owned())),
                hb.join().unwrap_or_else(|_| Err("panic".to_owned())),
            )
        });
        let (ta, tb) = match (ta, tb) {
            (Ok(ta), Ok(tb)) => (ta, tb),
            (ta, tb) => {
                for error in [ta.err(), tb.err()].into_iter().flatten() {
                    report.push_str(&format!("run {}: error: {error}\n", run + 1));
                }
                passed = false;
                continue;
            }
        };
        let label = |side: &Binary| format!("{} (run {})", side.label, run + 1);
        let ab = diff(&ta, &tb, &label(&a), &label(&b));
        if !ab.is_empty() {
            passed = false;
            report.push_str(&ab);
        }
        if let Some(expected) = &golden {
            for (side, transcript) in [(&a, &ta), (&b, &tb)] {
                let gd = diff(expected, transcript, "golden", &label(side));
                if !gd.is_empty() {
                    passed = false;
                    report.push_str(&gd);
                }
            }
        }
        match &first {
            None => {
                if let Some(dir) = comparison.record {
                    let path = dir.join(format!("{}.txt", scenario.name));
                    let mut text = ta.join("\n");
                    text.push('\n');
                    if let Err(error) =
                        fs::create_dir_all(dir).and_then(|()| fs::write(&path, text))
                    {
                        passed = false;
                        report.push_str(&format!("cannot record {}: {error}\n", path.display()));
                    }
                }
                first = Some((ta, tb));
            }
            Some((fa, fb)) => {
                for (side, earlier, now) in [(&a, fa, &ta), (&b, fb, &tb)] {
                    let rd = diff(
                        earlier,
                        now,
                        &format!("{} (run 1)", side.label),
                        &label(side),
                    );
                    if !rd.is_empty() {
                        passed = false;
                        report.push_str(&format!("nondeterministic {}:\n{rd}", side.label));
                    }
                }
            }
        }
        if !passed {
            break;
        }
    }
    if passed && let Some((ta, _)) = &first {
        report = ta.join("\n");
    }
    Outcome {
        name: scenario.name.clone(),
        passed,
        report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_captures_need_an_existing_file_under_captures() {
        let text = "name: r\ncapture: file:rec\nsteps: [{ capture: { from: c } }]\n";
        let scenario = Scenario::parse(text, Path::new("r.yaml")).expect("parse");
        let dir = std::env::temp_dir().join(format!("ms-harness-cap-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let mut options = RunOptions {
            html: dir.clone(),
            workroot: dir.clone(),
            captures: None,
        };
        let error = capture_path(&scenario, &options).expect_err("no --captures");
        assert!(error.contains("--captures"), "{error}");
        options.captures = Some(dir.clone());
        let error = capture_path(&scenario, &options).expect_err("missing file");
        assert!(error.contains("not found"), "{error}");
        fs::write(dir.join("rec.ts"), [0x47u8; 188 * 4]).expect("write");
        assert_eq!(capture_path(&scenario, &options), Ok(dir.join("rec.ts")));
        let replay = replay(&scenario, &dir.join("rec.ts")).expect("replay");
        assert_eq!(replay.loop_ms, 188 * 4 * 8 * 1000 / scenario.bitrate);
        let _ = fs::remove_dir_all(&dir);
    }
}
