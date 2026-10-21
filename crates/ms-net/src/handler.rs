//! RTSP/HTTP dispatch with session, stream and adapter decisions behind a trait.

use crate::response::{PUBLIC_HEADER, Response, format_http_date};
use crate::rtsp::{Protocol, Request, parse_int_prefix};
use std::time::SystemTime;

/// Maximum number of concurrent streams, matching C++ `MAX_STREAMS`.
pub const MAX_STREAMS: i32 = 256;

/// Index of a stream in a backend's table.
pub type StreamId = i32;

/// Stream output type, mirroring the C++ `STREAM_*` values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StreamType {
    /// No transport assigned yet.
    #[default]
    None,
    /// HTTP streaming.
    Http,
    /// RTP over UDP.
    RtspUdp,
    /// RTP over the RTSP TCP connection.
    RtspTcp,
    /// SRT.
    RtspSrt,
}

/// Information a backend exposes when building a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// Current output transport.
    pub stream_type: StreamType,
    /// RTP session id reported in `Session` headers.
    pub session_id: i64,
    /// One-based stream number reported in `com.ses.streamID`.
    pub stream_number: i32,
    /// Current destination host or client address.
    pub remote_host: String,
    /// Current destination or client RTP port.
    pub remote_port: u16,
    /// Local RTP server port.
    pub server_port: u16,
    /// Local RTCP server port.
    pub rtcp_server_port: u16,
    /// RTP sequence number used by `RTP-Info`.
    pub rtp_seq: u16,
    /// Stream timeout in milliseconds; `None` uses the server default.
    pub timeout_ms: Option<u32>,
}

impl Default for StreamInfo {
    fn default() -> Self {
        Self {
            stream_type: StreamType::None,
            session_id: 0,
            stream_number: 1,
            remote_host: String::new(),
            remote_port: 0,
            server_port: 0,
            rtcp_server_port: 0,
            rtp_seq: 0,
            timeout_ms: None,
        }
    }
}

/// Failure returned by [`StreamBackend::start_play`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayError {
    /// HTTP/RTSP status code.
    pub status: u16,
    /// Optional error body, emitted for 403 and 503.
    pub body: Option<String>,
}

impl PlayError {
    /// Creates an error with an optional body.
    pub fn new(status: u16, body: impl Into<Option<String>>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }
}

/// Error returned when a `Transport` header cannot be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportError;

/// A static HTTP resource served by a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResource {
    /// MIME type, e.g. `text/html`.
    pub content_type: String,
    /// Response body.
    pub body: Vec<u8>,
}

/// A fully formed HTTP response body and headers produced by a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRoute {
    /// Status code.
    pub status: u16,
    /// Raw header lines without the trailing CRLF.
    pub headers: Vec<String>,
    /// Response body.
    pub body: Vec<u8>,
    /// Whether a HEAD request still receives `body` (C++ generated pages do).
    pub body_on_head: bool,
    /// Whether the body is streamed the way `process_file` does after its
    /// first 8200-byte flush: no `Content-Length`, terminated by CRLFCRLF
    /// (utils.cpp:379-421).
    pub streamed: bool,
}

impl HttpRoute {
    /// Creates a route with no headers.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            body_on_head: false,
            streamed: false,
        }
    }

    /// Adds raw header lines.
    pub fn with_headers(mut self, headers: Vec<String>) -> Self {
        self.headers = headers;
        self
    }

    /// Marks the body as coming from the streamed `process_file` path.
    pub fn streaming(mut self) -> Self {
        self.streamed = true;
        self
    }
}

/// Session, stream and adapter decisions used by the RTSP handler.
pub trait StreamBackend {
    /// Resolves a `Session` header id to a live stream.
    fn find_session(&mut self, _session_id: i64) -> Option<StreamId> {
        None
    }

    /// Returns stream details, or `None` when the stream is not enabled.
    fn get_stream(&self, _id: StreamId) -> Option<StreamInfo> {
        None
    }

    /// Whether every stream slot is in use.
    fn streams_full(&self) -> bool {
        false
    }

    /// Creates or updates a stream for `url` and returns its id.
    fn setup_stream(&mut self, _url: &str, _current: Option<StreamId>) -> Option<StreamId> {
        None
    }

    /// [`StreamBackend::setup_stream`] variant that also sees the request method.
    fn setup_stream_for_request(
        &mut self,
        url: &str,
        current: Option<StreamId>,
        _method: &str,
    ) -> Option<StreamId> {
        self.setup_stream(url, current)
    }

    /// Forces a session id onto a stream.
    fn set_session_id(&mut self, _id: StreamId, _session_id: i64) {}

    /// Marks a stream as recently active.
    fn touch_stream(&mut self, _id: StreamId) {}

    /// Records the client user agent.
    fn set_user_agent(&mut self, _id: StreamId, _agent: &str) {}

    /// Disables the idle timeout for LIVE555/Lavf clients.
    fn disable_timeout(&mut self, _id: StreamId) {}

    /// Starts playback.
    fn start_play(&mut self, _id: StreamId, _protocol: Protocol) -> Result<(), PlayError> {
        Ok(())
    }

    /// Applies a `Transport` header; `Err` produces a 400 response.
    fn decode_transport(&mut self, _id: StreamId, _value: &str) -> Result<(), TransportError> {
        Ok(())
    }

    /// [`StreamBackend::decode_transport`] with the RTSP peer address.
    fn decode_transport_from(
        &mut self,
        id: StreamId,
        value: &str,
        _peer_host: &str,
        _peer_port: u16,
    ) -> Result<(), TransportError> {
        self.decode_transport(id, value)
    }

    /// [`StreamBackend::start_play`] with the client address.
    fn start_play_from(
        &mut self,
        id: StreamId,
        protocol: Protocol,
        _peer_host: &str,
        _peer_port: u16,
    ) -> Result<(), PlayError> {
        self.start_play(id, protocol)
    }

    /// Closes and disables a stream.
    fn close_stream(&mut self, _id: StreamId) {}

    /// Builds SDP for DESCRIBE, or `None` when no stream matches.
    fn describe_streams(&mut self, _current: Option<StreamId>, _url: &str) -> Option<String> {
        None
    }

    /// Returns a static HTTP resource.
    fn http_resource(&mut self, _uri: &str) -> Option<HttpResource> {
        None
    }

    /// Serves a generated or file-backed HTTP resource, including raw headers.
    fn http_route(&mut self, _request: &Request) -> Option<HttpRoute> {
        None
    }
}

/// Per-request state shared by the RTSP and HTTP handlers.
pub struct HandlerContext<'a, B: StreamBackend> {
    /// Session and stream decisions.
    pub backend: &'a mut B,
    /// Connection protocol.
    pub protocol: Protocol,
    /// Stream currently attached to the socket.
    pub sid: Option<StreamId>,
    /// Remote peer address.
    pub peer_host: String,
    /// Remote peer port.
    pub peer_port: u16,
    /// Local address used in `Content-Base` and `source=`.
    pub local_host: String,
    /// Preformatted `Date` header value.
    pub date: String,
    /// `Server` header value.
    pub server: String,
    /// Default stream timeout in milliseconds.
    pub timeout_ms: u32,
    /// First RTP port assigned to streams.
    pub start_rtp: u16,
    /// RTSP listening port reported for SRT.
    pub rtsp_port: u16,
    /// Value reported in `RTP-Info` as `rtptime`.
    pub rtptime_seconds: i64,
}

impl<'a, B: StreamBackend> HandlerContext<'a, B> {
    /// Creates a context with production defaults and the current time.
    pub fn new(backend: &'a mut B, protocol: Protocol) -> Self {
        Self {
            backend,
            protocol,
            sid: None,
            peer_host: "127.0.0.1".to_owned(),
            peer_port: 0,
            local_host: "127.0.0.1".to_owned(),
            date: format_http_date(SystemTime::now()),
            server: format!("minisatip/{}", env!("CARGO_PKG_VERSION")),
            timeout_ms: 30_000,
            start_rtp: 5500,
            rtsp_port: 554,
            rtptime_seconds: 0,
        }
    }

    /// Overrides the `Date` header value for deterministic output.
    pub fn with_date(mut self, date: impl Into<String>) -> Self {
        self.date = date.into();
        self
    }

    /// Overrides the `Server` header value.
    pub fn with_server(mut self, server: impl Into<String>) -> Self {
        self.server = server.into();
        self
    }
}

/// Dispatches a parsed RTSP request. `None` means the request is dropped silently.
pub fn handle_rtsp<B: StreamBackend>(
    ctx: &mut HandlerContext<'_, B>,
    request: &Request,
) -> Option<Response> {
    if request.is_method("GET") && ctx.protocol.is_rtsp() {
        return Some(base_response(ctx, 404));
    }

    let mut session_id = 0i64;
    if ctx.sid.is_none()
        && let Some(session) = request.session()
    {
        session_id = session.id;
        ctx.sid = ctx.backend.find_session(session.id);
    }

    if request.uri.contains("freq") || request.uri.contains("pids") {
        setup_stream(ctx, &request.uri, &request.method);
    }
    if ctx.sid.is_none()
        && (request.is_method("PLAY") || request.is_method("GET") || request.is_method("SETUP"))
    {
        setup_stream(ctx, &request.uri, &request.method);
    }
    if let Some(id) = ctx.sid {
        ctx.backend.touch_stream(id);
    }
    if session_id != 0
        && let Some(id) = ctx.sid
    {
        ctx.backend.set_session_id(id, session_id);
    }

    let mut cseq = 0;
    let mut transport = None;
    let mut user_agent = None;
    for header in &request.headers {
        if header.is("CSeq") {
            cseq = parse_int_prefix(&header.value, 0);
        } else if header.is("Transport") {
            transport = Some(header.value.as_str());
            let Some(id) = ctx.sid else {
                return Some(base_response(ctx, 400).with_cseq(cseq));
            };
            let peer = (ctx.peer_host.as_str(), ctx.peer_port);
            if ctx
                .backend
                .decode_transport_from(id, &header.value, peer.0, peer.1)
                .is_err()
            {
                return Some(base_response(ctx, 400).with_cseq(cseq));
            }
        } else if header.value.contains("LIVE555") || header.value.contains("Lavf") {
            if let Some(id) = ctx.sid {
                ctx.backend.disable_timeout(id);
            }
        } else if header.is("User-Agent") || (user_agent.is_none() && header.is("Server")) {
            // header_parameter keeps only the first whitespace token
            // (utils.cpp:145-172; minisatip.cpp:1297-1301).
            user_agent = Some(header.value.split_whitespace().next().unwrap_or(""));
        }
    }

    if request.is_method("PLAY") || request.is_method("GET") || request.is_method("SETUP") {
        let stream = ctx
            .sid
            .and_then(|id| ctx.backend.get_stream(id).map(|info| (id, info)));
        let Some((id, info)) = stream else {
            if ctx.sid.is_none() && session_id == 0 && ctx.backend.streams_full() {
                return Some(
                    base_response(ctx, 503)
                        .with_cseq(cseq)
                        .with_headers(vec!["Content-Type: text/parameters".to_owned()])
                        .with_body(b"No-More: sessions".to_vec()),
                );
            }
            return Some(base_response(ctx, 454).with_cseq(cseq));
        };
        if let Some(agent) = user_agent {
            ctx.backend.set_user_agent(id, agent);
        }
        if (request.is_method("PLAY") || request.is_method("GET"))
            && let Err(error) =
                ctx.backend
                    .start_play_from(id, ctx.protocol, &ctx.peer_host, ctx.peer_port)
        {
            let body = error.body.filter(|body| !body.is_empty());
            let mut response = base_response(ctx, error.status).with_cseq(cseq);
            if body.is_some() && (error.status == 403 || error.status == 503) {
                response = response
                    .with_headers(vec!["Content-Type: text/parameters".to_owned()])
                    .with_body(body.unwrap_or_default().into_bytes());
            }
            return Some(response);
        }

        let mut headers = Vec::new();
        if transport.is_some_and(|value| !value.is_empty()) {
            if let Some(line) = transport_line(ctx, &info) {
                headers.push(line);
            }
            if info.stream_type != StreamType::Http && info.stream_type != StreamType::None {
                let timeout = info
                    .timeout_ms
                    .filter(|timeout| *timeout > 20_000)
                    .unwrap_or(ctx.timeout_ms)
                    / 1000;
                headers.push(format!(
                    "Session: {:010};timeout={}",
                    info.session_id, timeout
                ));
                headers.push(format!("com.ses.streamID: {}", info.stream_number));
            }
        }
        if request.is_method("PLAY") {
            let url = request
                .uri
                .split('?')
                .next()
                .unwrap_or("")
                .split(['\r', '\n'])
                .next()
                .unwrap_or("");
            headers.push(format!(
                "RTP-Info: url={};seq={};rtptime={}",
                url, info.rtp_seq, ctx.rtptime_seconds
            ));
            headers.push("Range: npt=0.000-".to_owned());
        }
        if headers.is_empty() && info.stream_type == StreamType::Http {
            headers.push("Content-Type: video/mp2t".to_owned());
            headers.push("Connection: close".to_owned());
        }
        return Some(
            base_response(ctx, 200)
                .with_cseq(cseq)
                .with_headers(headers),
        );
    }

    if request.is_method("TEARDOWN") {
        let Some(info) = ctx.sid.and_then(|id| ctx.backend.get_stream(id)) else {
            return Some(base_response(ctx, 454).with_cseq(cseq));
        };
        let response = base_response(ctx, 200)
            .with_cseq(cseq)
            .with_headers(vec![format!("Session: {:010}", info.session_id)]);
        if let Some(id) = ctx.sid {
            ctx.backend.close_stream(id);
        }
        return Some(response);
    }

    if request.is_method("DESCRIBE") {
        if ctx.sid.is_none() && request.uri.contains('?') {
            setup_stream(ctx, &request.uri, &request.method);
        }
        let Some(sdp) = ctx.backend.describe_streams(ctx.sid, &request.uri) else {
            return Some(base_response(ctx, 404).with_cseq(cseq));
        };
        return Some(
            base_response(ctx, 200)
                .with_cseq(cseq)
                .with_headers(vec![
                    "Content-type: application/sdp".to_owned(),
                    format!("Content-Base: rtsp://{}/", ctx.local_host),
                ])
                .with_body(sdp.into_bytes()),
        );
    }

    if request.is_method("OPTIONS") {
        return Some(
            base_response(ctx, 200)
                .with_cseq(cseq)
                .with_headers(vec![PUBLIC_HEADER.to_owned()]),
        );
    }

    Some(
        base_response(ctx, 501)
            .with_cseq(cseq)
            .with_headers(vec![PUBLIC_HEADER.to_owned()]),
    )
}

/// Dispatches a parsed HTTP request. `None` means the request is dropped silently.
pub fn handle_http<B: StreamBackend>(
    ctx: &mut HandlerContext<'_, B>,
    request: &Request,
) -> Option<Response> {
    // read_http: `split(buf,' ').size() < 2` replies 503 before any routing
    // (minisatip.cpp:1561-1564).
    if request.uri.is_empty()
        || !(request.is_method("GET") || request.is_method("POST") || request.is_method("HEAD"))
    {
        return Some(base_response(ctx, 503));
    }
    if request.is_method("HEAD") && request.uri.contains("/?") {
        return Some(base_response(ctx, 200));
    }
    if request.is_method("GET") && request.uri.contains("/?") {
        return handle_rtsp(ctx, request);
    }

    if let Some(route) = ctx.backend.http_route(request) {
        let mut response = base_response(ctx, route.status).with_headers(route.headers);
        if route.streamed && !request.is_method("HEAD") {
            response = response.with_body(route.body).streamed();
        } else if route.body_on_head || !request.is_method("HEAD") {
            response = response.with_body(route.body);
        }
        return Some(response);
    }

    let Some(resource) = ctx.backend.http_resource(&request.uri) else {
        return Some(base_response(ctx, 404));
    };
    let headers = if resource.content_type.is_empty() {
        Vec::new()
    } else {
        vec![format!("Content-Type: {}", resource.content_type)]
    };
    let mut response = base_response(ctx, 200).with_headers(headers);
    if !request.is_method("HEAD") {
        response = response.with_body(resource.body);
    }
    Some(response)
}

fn setup_stream<B: StreamBackend>(ctx: &mut HandlerContext<'_, B>, url: &str, method: &str) {
    if let Some(id) = ctx.backend.setup_stream_for_request(url, ctx.sid, method) {
        ctx.sid = Some(id);
    }
}

fn base_response<B: StreamBackend>(ctx: &HandlerContext<'_, B>, status: u16) -> Response {
    let session = ctx
        .sid
        .and_then(|id| ctx.backend.get_stream(id))
        .map(|info| info.session_id);
    Response::new(ctx.protocol, status, ctx.date.clone(), ctx.server.clone()).with_session(session)
}

fn transport_line<B: StreamBackend>(
    ctx: &HandlerContext<'_, B>,
    info: &StreamInfo,
) -> Option<String> {
    match info.stream_type {
        StreamType::RtspUdp => {
            let multicast = first_octet(&info.remote_host) >= 224;
            if multicast {
                Some(format!(
                    "Transport: RTP/AVP;multicast;destination={};port={}-{}",
                    info.remote_host,
                    info.remote_port,
                    info.remote_port.wrapping_add(1)
                ))
            } else {
                Some(format!(
                    "Transport: RTP/AVP;unicast;destination={};source={};client_port={}-{};server_port={}-{}",
                    info.remote_host,
                    ctx.local_host,
                    info.remote_port,
                    info.remote_port.wrapping_add(1),
                    info.server_port,
                    info.server_port.wrapping_add(1)
                ))
            }
        }
        StreamType::RtspTcp => Some("Transport: RTP/AVP/TCP;interleaved=0-1".to_owned()),
        StreamType::RtspSrt => Some(format!("Transport: SRT/AVP;server_port={}", ctx.rtsp_port)),
        StreamType::Http | StreamType::None => None,
    }
}

fn first_octet(host: &str) -> i32 {
    let first = host.split('.').next().unwrap_or("");
    parse_int_prefix(first, 0)
}
