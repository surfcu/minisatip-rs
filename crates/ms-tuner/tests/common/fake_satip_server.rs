//! Port of tests/fake_satip_server.h: a scripted fake SAT>IP server.
//!
//! The script maps the start of a client request to a canned reply; the
//! longest match wins, so a generic fallback ("PLAY ") can sit next to specific ones.

use std::collections::BTreeMap;

use ms_tuner::satipc::Client;

/// Ported from tests/fake_satip_server.h:19
#[derive(Default)]
pub struct FakeSatipServer {
    pub script: BTreeMap<String, String>,
    pub requests: Vec<String>,
    pending: String,
}

impl FakeSatipServer {
    pub fn new(script: &[(&str, &str)]) -> Self {
        Self {
            script: script
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..Self::default()
        }
    }

    fn find_reply(&self, req: &str) -> Option<&String> {
        let mut best = None;
        let mut best_len = 0;
        for (prefix, reply) in &self.script {
            if prefix.len() > best_len && req.starts_with(prefix.as_str()) {
                best = Some(reply);
                best_len = prefix.len();
            }
        }
        best
    }

    /// Runs one wave of pending client requests: records each and feeds the
    /// mapped reply back; `Err` when a request matches nothing in the script.
    pub fn pump(&mut self, client: &mut Client) -> Result<usize, String> {
        for data in client.take_sent() {
            self.pending.push_str(&String::from_utf8_lossy(&data));
        }
        let mut count = 0;
        while let Some(pos) = self.pending.find("\r\n\r\n") {
            let req: String = self.pending.drain(..pos + 4).collect();
            let Some(reply) = self.find_reply(&req).cloned() else {
                return Err(format!("fake server has no reply for: {req}"));
            };
            self.requests.push(req);
            client.reply(reply.as_bytes());
            count += 1;
        }
        Ok(count)
    }

    /// Pumps until the client goes quiet; -1 past 100 messages or on an
    /// unscripted request.
    pub fn run(&mut self, client: &mut Client) -> i32 {
        let mut total = 0;
        loop {
            match self.pump(client) {
                Ok(0) => return total,
                Ok(n) => {
                    total += n as i32;
                    if total > 100 {
                        return -1;
                    }
                }
                Err(error) => {
                    eprintln!("{error}");
                    return -1;
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn dump(&self) {
        for (i, req) in self.requests.iter().enumerate() {
            eprintln!("fake server request {i}: {req}");
        }
    }
}
