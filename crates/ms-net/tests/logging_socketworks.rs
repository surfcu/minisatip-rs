//! Exact text of the socket-table log lines ported from `socketworks.cpp`.
//!
//! The logger is process-wide and [`ms_log::init`] may only run once, so this
//! file deliberately holds a single test.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_net::socket::{SocketTable, SocketType};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        // Fixed so the assertions do not depend on the clock.
        Timestamp::new(5, 3, 14, 7, 9, 42)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(format!("syslog:{line}"));
    }

    fn stdout(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

#[test]
fn socket_table_add_and_remove_log_lines() {
    ms_log::set_thread_name("main");

    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_SOCKETWORKS,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let mut table = SocketTable::new();
    let id = table
        .add_handle(7, SocketType::Tcp, 3)
        .expect("socket added");
    assert!(table.remove(id));

    let prefix = "[05/03 14:07:09.042 main]: ";
    let expected = [
        format!("{prefix}sockets_add: handle 7 (type 1) returning socket sock 0 [:0] read: 0x0"),
        format!(
            "{prefix}sockets_del: sock 0 -> handle 7, sid 3, overflow bytes 0, total buffered bytes 0"
        ),
        format!("{prefix}sockets_del: sock 0 Last open socket is at index 1 current_handle 7"),
    ];
    assert_eq!(*lines.lock().unwrap(), expected);
}
