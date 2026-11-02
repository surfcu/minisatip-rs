//! `--cam-menu-snapshot` (diag-log only): after a CAM opens an MMI page by itself, walk
//! configured menu paths pinned to page hashes and log the pages they reach. No C++ equivalent.

use super::consts::*;
use super::device::CaDevice;
use super::resources::App;

/// Wait after the CAM's own page before the first path.
const START_DELAY_MS: i64 = 5_000;
/// Pause between two paths: the CAM's late close of the last one (~2 s) lands in it.
const GAP_MS: i64 = 4_000;
/// A page that does not arrive within this ends its path.
const PAGE_MS: i64 = 10_000;
/// A snapshot that cannot start (a busy MMI) within this is dropped.
const START_WAIT_MS: i64 = 300_000;
/// At most one snapshot per slot in this time, counted from its trigger.
pub const SNAPSHOT_MIN_GAP_MS: i64 = 600_000;

/// A scheduled or running snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Next action (start, next path, or page deadline), ms.
    at: i64,
    /// Give up starting after this, ms.
    deadline: i64,
    /// The first path was opened: leftover pages are closed unanswered.
    started: bool,
    path: usize,
    /// Next step of the path being walked; `None` between paths.
    step: Option<usize>,
}

impl CaDevice {
    /// The CAM opened a page by itself: schedules a snapshot unless one was in the last 10 min.
    pub(super) fn snapshot_trigger(&mut self) {
        if self.snapshot_paths.is_empty() || self.snapshot.is_some() {
            return;
        }
        let now = self.now();
        if self.snapshot_last >= 0 && now - self.snapshot_last < SNAPSHOT_MIN_GAP_MS {
            return;
        }
        self.snapshot_last = now;
        ms_log::log!(
            "CA {}: CAM status snapshot of {} paths in {} s",
            self.id,
            self.snapshot_paths.len(),
            START_DELAY_MS / 1000
        );
        self.snapshot = Some(Snapshot {
            at: now + START_DELAY_MS,
            deadline: now + START_WAIT_MS,
            started: false,
            path: 0,
            step: None,
        });
    }

    /// The snapshot has opened a path: an unsolicited page is our walk's leftover.
    pub(super) fn snapshot_started(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.started)
    }

    /// A path is being walked: requested pages belong to the snapshot.
    pub(super) fn snapshot_walking(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.step.is_some())
    }

    /// A `--cam-menu` command from a user takes over the MMI.
    pub(super) fn snapshot_cancel(&mut self) {
        if self.snapshot.take().is_some() {
            ms_log::log!(
                "CA {}: CAM status snapshot cancelled by a menu request",
                self.id
            );
        }
    }

    /// The 1 s tick: starts the next path, or ends a path whose page did not come.
    pub(super) fn snapshot_tick(&mut self, now: i64) {
        let Some(snap) = self.snapshot.clone() else {
            return;
        };
        if now < snap.at {
            return;
        }
        if snap.step.is_some() {
            ms_log::log!(
                "CA {}: CAM status snapshot path {}: no page for {} ms, closing",
                self.id,
                snap.path + 1,
                PAGE_MS
            );
            self.snapshot_end_path(true);
            return;
        }
        if snap.path >= self.snapshot_paths.len() {
            ms_log::log!("CA {}: CAM status snapshot done", self.id);
            self.snapshot = None;
            return;
        }
        let busy = self.menu_open() || self.mmi_hold_ms >= 0;
        let Some(idx) = self.session_for_app(App::AppInfo).filter(|_| !busy) else {
            if now >= snap.deadline {
                ms_log::log!("CA {}: CAM status snapshot dropped: MMI busy", self.id);
                self.snapshot = None;
            }
            return;
        };
        ms_log::log!(
            "CA {}: CAM status snapshot path {} of {}",
            self.id,
            snap.path + 1,
            self.snapshot_paths.len()
        );
        if !self.write_apdu(idx, TAG_ENTER_MENU, &[]) {
            self.snapshot = None;
            return;
        }
        self.menu_request_ms = now;
        self.mmi_hold_ms = -1;
        self.mmi_page = None;
        if let Some(s) = self.snapshot.as_mut() {
            s.started = true;
            s.step = Some(0);
            s.at = now + PAGE_MS;
        }
    }

    /// A page of the walk: answered only when it is the step's pinned menu page; any other
    /// page is the target (all steps done) or ends the path. Closes in both cases.
    pub(super) fn snapshot_page(&mut self, idx: usize, menu_items: Option<usize>, hash: u64) {
        let now = self.now();
        let Some(snap) = self.snapshot.as_mut() else {
            return;
        };
        let (Some(step), Some(path)) = (snap.step, self.snapshot_paths.get(snap.path)) else {
            return;
        };
        match path.get(step) {
            Some(&(want, choice))
                if hash == want && menu_items.is_some_and(|n| usize::from(choice) <= n) =>
            {
                snap.step = Some(step + 1);
                snap.at = now + PAGE_MS;
                ms_log::log!(
                    "CA {}: CAM status snapshot path {} step {}: choice {}",
                    self.id,
                    snap.path + 1,
                    step + 1,
                    choice
                );
                self.write_apdu(idx, TAG_MENU_ANSWER, &[choice]);
                self.menu_request_ms = now;
                self.mmi_page = None;
            }
            Some(_) => {
                ms_log::log!(
                    "CA {}: CAM status snapshot path {} stopped: page {:016x} is not step {}",
                    self.id,
                    snap.path + 1,
                    hash,
                    step + 1
                );
                self.snapshot_end_path(true);
            }
            None => {
                ms_log::log!(
                    "CA {}: CAM status snapshot path {} read",
                    self.id,
                    snap.path + 1
                );
                self.snapshot_end_path(true);
            }
        }
    }

    /// Ends the walked path (closing the MMI when `close`) and schedules the next one.
    pub(super) fn snapshot_end_path(&mut self, close: bool) {
        if close && let Some(idx) = self.session_for_app(App::Mmi) {
            self.write_apdu(idx, TAG_CLOSE_MMI, &[MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
        }
        self.end_menu();
        let now = self.now();
        if let Some(s) = self.snapshot.as_mut() {
            s.path += 1;
            s.step = None;
            s.at = now + GAP_MS;
        }
    }

    /// The CAM closed the MMI, or its session went away, during a walk.
    pub(super) fn snapshot_closed(&mut self) {
        if self.snapshot_walking() {
            ms_log::log!("CA {}: CAM status snapshot path closed by the CAM", self.id);
            self.snapshot_end_path(false);
        }
    }
}
