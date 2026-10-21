//! SSDP message and DEVICEID conflict tests.

use std::fs;
use std::io;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use ms_net::ssdp::{
    SsdpConfig, SsdpIo, SsdpServer, device_id_conflict, notify_messages, read_boot_id,
    search_reply, st_matches,
};

const UUID: &str = "1234-5678";
const DATE: &str = "Wed, 21 Oct 2026 10:00:00 GMT";

fn config() -> SsdpConfig {
    SsdpConfig {
        uuid: UUID.to_owned(),
        device_id: 2,
        bootid: 3,
        name_app: "minisatip".to_owned(),
        version: "1.2.3".to_owned(),
        http_host: "10.0.0.5:8080".to_owned(),
        xml_path: "desc.xml".to_owned(),
        disc_host: "239.255.255.250".to_owned(),
        local_ip: "10.0.0.5".to_owned(),
        cache_dir: None,
        disabled: false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    data: Vec<u8>,
    host: String,
    port: u16,
}

#[derive(Default)]
struct FakeIo {
    sent: Mutex<Vec<Sent>>,
}

impl SsdpIo for FakeIo {
    fn send_to(&mut self, data: &[u8], host: &str, port: u16) -> io::Result<usize> {
        self.sent.lock().expect("sent").push(Sent {
            data: data.to_vec(),
            host: host.to_owned(),
            port,
        });
        Ok(data.len())
    }
}

fn sent(server: &SsdpServer<FakeIo>) -> Vec<Sent> {
    server.io().sent.lock().expect("sent").clone()
}

fn search(device_id: i32, st: &str) -> Vec<u8> {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nST: {st}\r\nDEVICEID.SES.COM: {device_id}\r\n\r\n"
    )
    .into_bytes()
}

#[test]
fn notify_messages_match_cpp_format() {
    // Expected literals derived from minisatip.cpp:1720-1736.
    let messages = notify_messages(&config(), true);
    assert_eq!(messages.len(), 3);
    assert_eq!(
        String::from_utf8(messages[0].clone()).unwrap(),
        concat!(
            "NOTIFY * HTTP/1.1\r\n",
            "HOST: 239.255.255.250:1900\r\n",
            "CACHE-CONTROL: max-age=1800\r\n",
            "LOCATION: http://10.0.0.5:8080/desc.xml\r\n",
            "NT: upnp:rootdevice\r\n",
            "NTS: ssdp:alive\r\n",
            "SERVER: Linux/1.0 UPnP/1.1 minisatip/1.2.3\r\n",
            "USN: uuid:1234-5678::upnp:rootdevice\r\n",
            "BOOTID.UPNP.ORG: 3\r\n",
            "CONFIGID.UPNP.ORG: 0\r\n",
            "DEVICEID.SES.COM: 2\r\n",
            "\r\n"
        )
    );
    assert_eq!(
        String::from_utf8(messages[1].clone()).unwrap(),
        concat!(
            "NOTIFY * HTTP/1.1\r\n",
            "HOST: 239.255.255.250:1900\r\n",
            "CACHE-CONTROL: max-age=1800\r\n",
            "LOCATION: http://10.0.0.5:8080/desc.xml\r\n",
            "NT: uuid:1234-5678\r\n",
            "NTS: ssdp:alive\r\n",
            "SERVER: Linux/1.0 UPnP/1.1 minisatip/1.2.3\r\n",
            "USN: uuid:1234-5678\r\n",
            "BOOTID.UPNP.ORG: 3\r\n",
            "CONFIGID.UPNP.ORG: 0\r\n",
            "DEVICEID.SES.COM: 2\r\n",
            "\r\n"
        )
    );
    assert_eq!(
        String::from_utf8(messages[2].clone()).unwrap(),
        concat!(
            "NOTIFY * HTTP/1.1\r\n",
            "HOST: 239.255.255.250:1900\r\n",
            "CACHE-CONTROL: max-age=1800\r\n",
            "LOCATION: http://10.0.0.5:8080/desc.xml\r\n",
            "NT: urn:ses-com:device:SatIPServer:1\r\n",
            "NTS: ssdp:alive\r\n",
            "SERVER: Linux/1.0 UPnP/1.1 minisatip/1.2.3\r\n",
            "USN: uuid:1234-5678::urn:ses-com:device:SatIPServer:1\r\n",
            "BOOTID.UPNP.ORG: 3\r\n",
            "CONFIGID.UPNP.ORG: 0\r\n",
            "DEVICEID.SES.COM: 2\r\n",
            "\r\n"
        )
    );

    let bye = notify_messages(&config(), false);
    assert_eq!(
        String::from_utf8(bye[0].clone()).unwrap(),
        concat!(
            "NOTIFY * HTTP/1.1\r\n",
            "HOST: 239.255.255.250:1900\r\n",
            "LOCATION: http://10.0.0.5:8080/desc.xml\r\n",
            "NT: upnp:rootdevice\r\n",
            "NTS: ssdp:byebye\r\n",
            "USN: uuid:1234-5678::upnp:rootdevice\r\n",
            "BOOTID.UPNP.ORG: 3\r\n",
            "CONFIGID.UPNP.ORG: 0\r\n",
            // C++ omits DEVICEID for byebye but still appends "\r\n\r\n".
            "\r\n",
            "\r\n"
        )
    );
}

#[test]
fn search_reply_and_conflict_match_cpp_format() {
    // minisatip.cpp:1764-1774 and 1775-1780.
    assert_eq!(
        String::from_utf8(search_reply(&config(), DATE, 2)).unwrap(),
        concat!(
            "HTTP/1.1 200 OK\r\n",
            "CACHE-CONTROL: max-age=1800\r\n",
            "DATE: Wed, 21 Oct 2026 10:00:00 GMT\r\n",
            "EXT:\r\n",
            "LOCATION: http://10.0.0.5:8080/desc.xml\r\n",
            "SERVER: Linux/1.0 UPnP/1.1 minisatip/1.2.3\r\n",
            "ST: urn:ses-com:device:SatIPServer:1\r\n",
            "USN: uuid:1234-5678::urn:ses-com:device:SatIPServer:1\r\n",
            "BOOTID.UPNP.ORG: 3\r\n",
            "CONFIGID.UPNP.ORG: 0\r\n",
            "DEVICEID.SES.COM: 2\r\n",
            "\r\n"
        )
    );
    assert_eq!(
        String::from_utf8(device_id_conflict(&config())).unwrap(),
        concat!(
            "M-SEARCH * HTTP/1.1\r\n",
            "HOST: 10.0.0.5:1900\r\n",
            "MAN: \"ssdp:discover\"\r\n",
            "ST: urn:ses-com:device:SatIPServer:1\r\n",
            "USER-AGENT: Linux/1.0 UPnP/1.1 minisatip/1.2.3\r\n",
            "DEVICEID.SES.COM: 2\r\n",
            "\r\n"
        )
    );
}

#[test]
fn st_matching_covers_all_targets() {
    assert!(st_matches("ssdp:all", UUID));
    assert!(st_matches("urn:ses-com:device:SatIPServer:1", UUID));
    assert!(st_matches("upnp:rootdevice", UUID));
    assert!(st_matches(&format!("uuid:{UUID}"), UUID));
    assert!(!st_matches(
        "urn:schemas-upnp-org:device:MediaServer:1",
        UUID
    ));
    assert!(!st_matches("uuid:other", UUID));
}

#[test]
fn announce_sends_once_to_multicast() {
    let mut server = SsdpServer::new(FakeIo::default(), config());
    assert!(server.announce(0).expect("announce"));
    assert!(!server.announce(1).expect("already announced"));
    let messages = sent(&server);
    assert_eq!(messages.len(), 3);
    assert!(
        messages
            .iter()
            .all(|message| message.host == "239.255.255.250" && message.port == 1900)
    );
}

#[test]
fn m_search_with_matching_st_gets_reply() {
    let mut server = SsdpServer::new(FakeIo::default(), config());
    let action = server
        .handle(
            &search(1, "urn:ses-com:device:SatIPServer:1"),
            "10.0.0.9",
            40000,
            DATE,
            0,
        )
        .expect("handle");
    assert!(action.replied);
    assert!(!action.device_id_conflict);
    let messages = sent(&server);
    assert_eq!(messages.len(), 4);
    assert_eq!(
        messages[3],
        Sent {
            data: search_reply(&config(), DATE, 2),
            host: "10.0.0.9".to_owned(),
            port: 40000,
        }
    );
}

#[test]
fn unknown_st_gets_a_reply_like_cpp() {
    // C++ never inspects ST before replying (minisatip.cpp:1848-1865).
    let mut server = SsdpServer::new(FakeIo::default(), config());
    let action = server
        .handle(
            &search(1, "urn:schemas-upnp-org:device:MediaServer:1"),
            "10.0.0.9",
            40000,
            DATE,
            0,
        )
        .expect("handle");
    assert!(action.replied);
    assert_eq!(sent(&server).len(), 4);
}

#[test]
fn discovery_without_st_gets_a_reply() {
    let mut server = SsdpServer::new(FakeIo::default(), config());
    let message = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\n\r\n";
    let action = server
        .handle(message, "10.0.0.9", 40000, DATE, 0)
        .expect("handle");
    assert!(action.replied);
    assert_eq!(sent(&server).len(), 4);
}

#[test]
fn messages_from_our_uuid_are_dropped() {
    let mut server = SsdpServer::new(FakeIo::default(), config());
    let mut message = search(1, "ssdp:all");
    message.extend_from_slice(b"USN: uuid:1234-5678::upnp:rootdevice\r\n");
    let action = server
        .handle(&message, "10.0.0.9", 40000, DATE, 0)
        .expect("handle");
    assert!(!action.replied);
    assert_eq!(sent(&server).len(), 3);
}

#[test]
fn notify_with_same_device_id_requests_a_change() {
    let mut server = SsdpServer::new(FakeIo::default(), config());
    let message = b"NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nDEVICEID.SES.COM: 2\r\n\r\n";
    let action = server
        .handle(message, "10.0.0.8", 1900, DATE, 0)
        .expect("handle");
    assert!(action.device_id_conflict);
    assert!(!action.replied);
    assert_eq!(server.config().device_id, 2, "NOTIFY does not increment");
    let messages = sent(&server);
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[3].data, device_id_conflict(&config()));
    assert_eq!(messages[3].host, "10.0.0.8");
    assert_eq!(messages[3].port, 1900);
}

#[test]
fn device_id_clash_in_search_increments_and_replies_with_old_id() {
    // minisatip.cpp:1833-1853.
    let mut server = SsdpServer::new(FakeIo::default(), config());
    let action = server
        .handle(
            &search(2, "urn:ses-com:device:SatIPServer:1"),
            "10.0.0.8",
            50000,
            DATE,
            0,
        )
        .expect("handle");
    assert!(action.device_id_conflict);
    assert!(action.replied);
    assert_eq!(server.config().device_id, 3);
    assert_eq!(server.config().bootid, 4);
    let mut expected_config = config();
    expected_config.device_id = 2; // reply reports the requested (old) id
    expected_config.bootid = 4;
    let messages = sent(&server);
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[3].data, search_reply(&expected_config, DATE, 2));
}

#[test]
fn device_id_clash_outside_window_is_ignored() {
    let mut server = SsdpServer::new(FakeIo::default(), config());
    let reply = b"HTTP/1.1 200 OK\r\nST: urn:ses-com:device:SatIPServer:1\r\n\r\n";
    server
        .handle(reply, "10.0.0.8", 1900, DATE, 0)
        .expect("announce");
    let action = server
        .handle(
            &search(2, "urn:ses-com:device:SatIPServer:1"),
            "10.0.0.8",
            50000,
            DATE,
            20_000,
        )
        .expect("handle");
    assert!(!action.device_id_conflict);
    assert!(action.replied);
    assert_eq!(server.config().device_id, 2);
}

#[test]
fn device_id_clash_persists_the_boot_id_file() {
    // C++ increments DEVICEID and calls readBootID() (minisatip.cpp:1844).
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("msnet-ssdp-conflict-{nanos}"));
    fs::create_dir_all(&dir).expect("cache dir");
    fs::write(dir.join("bootid"), format!("3 2 {UUID}")).expect("bootid");
    let mut config = config();
    config.cache_dir = Some(dir.clone());
    let mut server = SsdpServer::new(FakeIo::default(), config);
    let action = server
        .handle(
            &search(2, "urn:ses-com:device:SatIPServer:1"),
            "10.0.0.8",
            50000,
            DATE,
            0,
        )
        .expect("handle");
    assert!(action.device_id_conflict);
    assert_eq!(server.config().device_id, 3);
    assert_eq!(server.config().bootid, 4);
    assert_eq!(
        fs::read_to_string(dir.join("bootid")).expect("bootid"),
        format!("4 3 {UUID}")
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn disabled_ssdp_sends_nothing() {
    let mut config = config();
    config.disabled = true;
    let mut server = SsdpServer::new(FakeIo::default(), config);
    assert!(!server.announce(0).expect("announce"));
    let action = server
        .handle(&search(1, "ssdp:all"), "10.0.0.9", 40000, DATE, 0)
        .expect("handle");
    assert!(!action.replied);
    assert!(sent(&server).is_empty());
    server.byebye().expect("byebye");
    assert!(sent(&server).is_empty());
}

#[test]
fn boot_id_file_is_incremented_and_device_id_kept() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("msnet-ssdp-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("cache dir");
    let mut uuid = String::new();
    let (bootid, device_id) = read_boot_id(&dir, 0, &mut uuid).expect("read boot id");
    assert_eq!((bootid, device_id), (1, 1));
    assert!(!uuid.is_empty());
    let stored = fs::read_to_string(dir.join("bootid")).expect("bootid file");
    assert!(stored.starts_with("1 1 "));
    let mut uuid = String::new();
    let (bootid, device_id) = read_boot_id(&dir, 5, &mut uuid).expect("read boot id");
    assert_eq!((bootid, device_id), (2, 5));
    fs::remove_dir_all(dir).ok();
}
