//! Fake CAM for EN50221 scenarios (`cam: true`): `ms-fakecam` on a unix datagram
//! socket that both servers reach through `MINISATIP_REPLAY_CA` (DEV-283).

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use ms_fakecam::record::Event;
use ms_fakecam::tags::TAG_DATE_TIME;
use ms_fakecam::{CamConfig, FakeCam, MonotonicClock, Recorder};

/// CA system id of the `ca` capture profile.
const CAID: u16 = 0x0100;
/// TPDU data limit, so no datagram exceeds the 4096-byte reads of both hosts.
const MAX_TPDU_DATA: usize = 4000;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// One fake CAM serving adapter 0 of one server run; stopped on drop.
pub struct HarnessCam {
    pub path: PathBuf,
    dir: PathBuf,
    recorder: Recorder,
}

impl HarnessCam {
    /// Binds a short socket path (`sun_path` holds 108 bytes) and serves it.
    pub fn start() -> std::io::Result<Self> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/msh-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("cam");
        let sock = UnixDatagram::bind(&path)?;
        let recorder = Recorder::new();
        let config = CamConfig {
            caids: vec![CAID],
            max_tpdu_data: Some(MAX_TPDU_DATA),
            ..CamConfig::default()
        };
        let cam = Arc::new(Mutex::new(FakeCam::with_recorder(
            config,
            Arc::new(MonotonicClock::default()),
            recorder.clone(),
        )));
        thread::spawn(move || {
            let _ = ms_fakecam::io::serve_datagram_listener(&cam, &sock);
        });
        Ok(Self {
            path,
            dir,
            recorder,
        })
    }

    /// The APDU log without times; DATE_TIME bodies carry the wall clock.
    pub fn lines(&self) -> Vec<String> {
        self.recorder
            .records()
            .iter()
            .map(|record| match &record.event {
                Event::Apdu { tag, .. } if *tag == TAG_DATE_TIME => {
                    let line = record.untimed();
                    match line.rfind(' ') {
                        Some(cut) => format!("{} <TIME>", &line[..cut]),
                        None => line,
                    }
                }
                _ => record.untimed(),
            })
            .collect()
    }
}

impl Drop for HarnessCam {
    fn drop(&mut self) {
        // An empty datagram ends the serving thread.
        if let Ok(sock) = UnixDatagram::unbound() {
            let _ = sock.send_to(&[], &self.path);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
