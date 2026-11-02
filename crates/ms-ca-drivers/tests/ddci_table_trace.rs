//! `--cam-ecm-trace`: ECM/EMM counts and ECM parity changes on the DDCI write path.

use ms_ca::ProgramTable;
use ms_ca_drivers::ddci::*;

#[derive(Default)]
struct Sink;

impl CiWriter for Sink {
    fn writev(&mut self, iov: &[&[u8]]) -> isize {
        iov.iter().map(|s| s.len()).sum::<usize>() as isize
    }
}

/// A payload packet on `pid`; with `table_id`, a section starts in it (after an adaptation field when `af`).
fn packet(pid: u16, table_id: Option<u8>, af: bool) -> Vec<u8> {
    let mut b = vec![0xFFu8; 188];
    b[0] = 0x47;
    set_pid_ts(&mut b, pid);
    b[1] = (b[1] & 0x1F) | if table_id.is_some() { 0x40 } else { 0 };
    let mut at = 4;
    if af {
        b[3] = 0x30;
        b[4] = 2;
        b[5] = 0;
        at = 7;
    } else {
        b[3] = 0x10;
    }
    if let Some(tid) = table_id {
        b[at] = 0;
        b[at + 1] = tid;
    }
    b
}

/// DDCI 2 in slot 0; adapter 1 carries ECM 1000, EMM 1001 and video 2000, all mapped.
fn fixture() -> (Ddci, ProgramTable) {
    let mut x = Ddci::new();
    x.devices.insert(0, DdciDevice::new(2, 0));
    for a in [0, 1, 2] {
        x.create_adapter(a);
    }
    for pid in [1000, 1001, 2000] {
        x.add_pid_mapping_table(1, pid, 0, 0, false);
    }
    let d = x.devices.get_mut(&0).unwrap();
    d.emm_pids.insert((1, 1001));
    d.read_index[1] = 188;
    d.fifo.write_index = 188;
    let mut t = ProgramTable::new();
    let id = t.add(1, 0x64, 0x100);
    t.get_mut(id).unwrap().add_caid(0x0100, 1000, &[]);
    (x, t)
}

fn feed(x: &mut Ddci, t: &mut ProgramTable, packets: &[Vec<u8>]) {
    let mut buf: Vec<u8> = packets.concat();
    let rlen = buf.len();
    buf.extend_from_slice(&[0u8; 188]);
    let mut ad = TsBuffer { id: 1, buf, rlen };
    x.process_ts(&mut ad, 0, t, &mut Sink);
}

#[test]
fn ecm_and_emm_packets_are_counted_and_parity_changes_reported() {
    let (mut x, mut t) = fixture();
    x.table_trace = Some(TableTrace::default());
    feed(
        &mut x,
        &mut t,
        &[
            packet(1000, Some(0x80), false),
            packet(1000, None, false),
            packet(1000, Some(0x80), true),
            packet(1001, Some(0x82), false),
            packet(2000, Some(0x02), false),
            packet(1000, Some(0x81), false),
        ],
    );
    let (counts, changes) = x.table_trace.as_mut().unwrap().take();
    assert_eq!(
        counts[&2],
        TableCounts {
            ecm_pkts: 4,
            emm_pkts: 1,
            ecm_sections: 3,
            emm_sections: 1,
            parity_dropped: 0,
        },
        "the video PID is not counted"
    );
    assert_eq!(
        changes.len(),
        1,
        "the first table_id is a baseline, not a change"
    );
    assert_eq!(
        (
            changes[0].dev,
            changes[0].ad,
            changes[0].pid,
            changes[0].table_id
        ),
        (2, 1, 1000, 0x81)
    );
    assert!(changes[0].wall_ms > 0);

    feed(&mut x, &mut t, &[packet(1000, Some(0x80), false)]);
    let (counts, changes) = x.table_trace.as_mut().unwrap().take();
    assert_eq!(counts[&2].ecm_pkts, 1, "counts restart after a drain");
    assert_eq!(changes.len(), 1, "the baseline survives the drain");
    assert_eq!(changes[0].table_id, 0x80);
}

#[test]
fn nothing_is_counted_without_the_option() {
    let (mut x, mut t) = fixture();
    feed(&mut x, &mut t, &[packet(1000, Some(0x80), false)]);
    assert!(x.table_trace.is_none());
}

#[test]
fn table_ids_are_read_only_where_a_section_really_starts() {
    let (mut x, mut t) = fixture();
    x.table_trace = Some(TableTrace::default());
    let mut pointer = packet(1000, None, false);
    pointer[1] |= 0x40;
    pointer[4] = 3; // pointer_field: the section starts 3 bytes on
    pointer[8] = 0x80;
    let mut long_af = packet(1000, Some(0x81), true);
    long_af[4] = 200; // the adaptation field runs past the packet
    let mut scrambled = packet(1000, Some(0x81), false);
    scrambled[3] |= 0x80;
    let stuffing = packet(1000, Some(0xFF), false);
    feed(&mut x, &mut t, &[pointer, long_af, scrambled, stuffing]);
    let (counts, changes) = x.table_trace.as_mut().unwrap().take();
    assert_eq!(counts[&2].ecm_pkts, 4);
    assert_eq!(
        counts[&2].ecm_sections, 1,
        "only the pointer_field packet starts a section"
    );
    assert!(
        changes.is_empty(),
        "0x80 was the baseline; nothing else was read"
    );
}

#[test]
fn parity_changes_over_the_cap_are_counted_per_device() {
    let (mut x, mut t) = fixture();
    x.table_trace = Some(TableTrace::default());
    let packets: Vec<Vec<u8>> = (0..1030)
        .map(|i| packet(1000, Some(if i % 2 == 0 { 0x80 } else { 0x81 }), false))
        .collect();
    feed(&mut x, &mut t, &packets);
    let (counts, changes) = x.table_trace.as_mut().unwrap().take();
    assert_eq!(changes.len(), 1024);
    assert_eq!(counts[&2].parity_dropped, 1029 - 1024);
}

#[test]
fn a_pid_that_stops_being_an_ecm_pid_loses_its_baseline() {
    let (mut x, mut t) = fixture();
    x.table_trace = Some(TableTrace::default());
    feed(&mut x, &mut t, &[packet(1000, Some(0x80), false)]);
    // A retune: PID 1000 is no ECM PID for one write, then is one again.
    let id = t.iter().next().unwrap().id;
    t.get_mut(id).unwrap().caids.clear();
    feed(&mut x, &mut t, &[packet(2000, None, false)]);
    t.get_mut(id).unwrap().add_caid(0x0100, 1000, &[]);
    feed(&mut x, &mut t, &[packet(1000, Some(0x81), false)]);
    let (_, changes) = x.table_trace.as_mut().unwrap().take();
    assert!(
        changes.is_empty(),
        "the old 0x80 is not compared with the new 0x81"
    );
}
