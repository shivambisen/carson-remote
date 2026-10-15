//! One remote-viewing session: a WebRTC peer connection that streams this machine's screen.
//!
//! Each session is an actor task owning its `PeerConnection` and media pipeline. Signaling
//! messages and connection-state changes arrive as [`SessionEvent`]s; outgoing signaling
//! goes to the shared outbox (the WebSocket writer).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use rtc::interceptor::Slot;
use rtc::media::Sample;
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_H264;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RtpCodecKind,
};
use tokio::sync::mpsc;
use webrtc::media_stream::MediaStreamTrack;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState, RTCSessionDescription, Registry, register_default_interceptors,
};

use carson_proto::input::{CONTROL_CHANNEL, POINTER_CHANNEL};
use carson_proto::{Envelope, MessageType};
use webrtc::data_channel::RTCDataChannelInit;

use crate::keyframe::{KeyframeRequestForwarder, is_keyframe_request};
use crate::media::{MediaOptions, PipelineControl, spawn_pipeline};

/// Outgoing signaling messages (drained by the WebSocket loop).
pub type Outbox = mpsc::UnboundedSender<Envelope>;

/// Sends a session-scoped message; a closed outbox means we are shutting down.
pub fn send(out: &Outbox, kind: MessageType, sid: &str, body: impl serde::Serialize) {
    match Envelope::new(kind, body) {
        Ok(mut env) => {
            env.sid = Some(sid.to_owned());
            let _ = out.send(env);
        }
        Err(e) => tracing::error!(error = %e, "serialize signaling message"),
    }
}

#[derive(Debug)]
pub enum SessionEvent {
    Answer(Box<RTCSessionDescription>),
    Candidate(RTCIceCandidateInit),
    Connected,
    /// The connection is gone; the reason is reported to the viewer.
    Failed(&'static str),
    /// The viewer (or server) ended the session.
    RemoteEnd,
    /// The viewer may see and control the screen (no PIN needed, or PIN accepted).
    Authorized,
}

/// Everything that defines one session.
pub struct SessionConfig {
    pub sid: String,
    pub ice_servers: Vec<RTCIceServer>,
    pub media: MediaOptions,
    /// Unattended-access PIN to verify before sharing anything.
    pub gate: Option<crate::control::PinGate>,
    /// "Being controlled" banner text. `None` only when nothing real is shared (test pattern).
    pub banner: Option<String>,
}

/// H.264 Constrained Baseline, which every browser decodes (often in hardware).
pub fn h264_codec() -> RTCRtpCodecParameters {
    let fb = |typ: &str, parameter: &str| RTCPFeedback {
        typ: typ.into(),
        parameter: parameter.into(),
    };
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                .to_owned(),
            rtcp_feedback: vec![
                fb("nack", ""),
                fb("nack", "pli"),
                fb("ccm", "fir"),
                fb("transport-cc", ""),
            ],
        },
        payload_type: 102,
    }
}

/// Builds a peer connection configured for carson (H.264 only, keyframe requests forwarded).
pub async fn build_peer(
    ice_servers: Vec<RTCIceServer>,
    handler: Arc<dyn PeerConnectionEventHandler>,
) -> Result<Arc<dyn PeerConnection>> {
    let mut media_engine = MediaEngine::default();
    media_engine.register_codec(h264_codec(), RtpCodecKind::Video)?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)?
        // Application-most, so the default interceptors see all inbound RTCP first.
        .with(Slot::from(14_000), KeyframeRequestForwarder::default());
    let config = RTCConfigurationBuilder::new()
        .with_ice_servers(ice_servers)
        .build();
    let pc = PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        // Wildcard = one socket per real interface, so host candidates work on the LAN.
        .with_udp_addrs(vec!["0.0.0.0:0"])
        .build()
        .await?;
    Ok(Arc::new(pc))
}

struct Handler {
    sid: String,
    out: Outbox,
    events: mpsc::UnboundedSender<SessionEvent>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        match event.candidate.to_json() {
            Ok(init) => send(&self.out, MessageType::IceCandidate, &self.sid, init),
            Err(e) => tracing::warn!(error = %e, "serialize local ICE candidate"),
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        tracing::info!(sid = %self.sid, %state, "peer connection state");
        let event = match state {
            RTCPeerConnectionState::Connected => SessionEvent::Connected,
            RTCPeerConnectionState::Failed => SessionEvent::Failed("connection_failed"),
            RTCPeerConnectionState::Closed => SessionEvent::Failed("closed"),
            // `Disconnected` can recover on its own; ICE restart arrives in M5.
            _ => return,
        };
        let _ = self.events.send(event);
    }
}

/// How long the viewer has to complete the WebRTC handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the viewer has to enter the unattended-access PIN once connected.
const PIN_ENTRY_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs one session until it ends. Always tells the viewer why it ended.
pub async fn run_session(
    cfg: SessionConfig,
    capturer: Box<dyn carson_capture::Capturer>,
    out: Outbox,
    events_tx: mpsc::UnboundedSender<SessionEvent>,
    mut events: mpsc::UnboundedReceiver<SessionEvent>,
) {
    let sid = cfg.sid.clone();
    let reason = match session_inner(&cfg, capturer, &out, events_tx, &mut events).await {
        Ok(reason) => reason,
        Err(e) => {
            tracing::error!(sid = %sid, error = %format!("{e:#}"), "session failed");
            "agent_error"
        }
    };
    if reason != "remote_end" {
        send(
            &out,
            MessageType::SessionEnd,
            &sid,
            serde_json::json!({ "reason": reason }),
        );
    }
    tracing::info!(sid = %sid, reason, "session ended");
}

async fn session_inner(
    cfg: &SessionConfig,
    capturer: Box<dyn carson_capture::Capturer>,
    out: &Outbox,
    events_tx: mpsc::UnboundedSender<SessionEvent>,
    events: &mut mpsc::UnboundedReceiver<SessionEvent>,
) -> Result<&'static str> {
    let (sid, media) = (cfg.sid.as_str(), &cfg.media);
    let handler = Arc::new(Handler {
        sid: sid.to_owned(),
        out: out.clone(),
        events: events_tx.clone(),
    });
    let pc = build_peer(cfg.ice_servers.clone(), handler)
        .await
        .context("create peer connection")?;

    let codec = h264_codec();
    let ssrc = random_u32();
    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "carson-screen".to_owned(),
            "carson-screen-video".to_owned(),
            "screen".to_owned(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec: codec.rtp_codec.clone(),
                ..Default::default()
            }],
        ),
    )?);
    let sender = pc
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
        .await?;

    // Created by the offerer so they are negotiated in the first offer.
    let ptr = pc
        .create_data_channel(
            POINTER_CHANNEL,
            Some(RTCDataChannelInit {
                ordered: false,
                max_retransmits: Some(0),
                ..Default::default()
            }),
        )
        .await?;
    let ctl_channel = pc.create_data_channel(CONTROL_CHANNEL, None).await?;
    let ctl = PipelineControl::default();
    let control_task = tokio::spawn(crate::control::run(
        ptr,
        ctl_channel,
        media.clone(),
        ctl.clone(),
        cfg.gate.clone(),
        events_tx,
    ));

    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer.clone()).await?;
    send(out, MessageType::SdpOffer, sid, &offer);

    let mut capturer = Some(capturer);
    let mut streaming: Option<tokio::task::JoinHandle<()>> = None;
    let mut banner: Option<crate::ui::Banner> = None;
    let (mut connected, mut authorized) = (false, false);
    let deadline = tokio::time::sleep(CONNECT_TIMEOUT);
    tokio::pin!(deadline);

    let reason = loop {
        let event = tokio::select! {
            ev = events.recv() => ev,
            _ = &mut deadline, if !(connected && authorized) => {
                break if connected { "pin_timeout" } else { "connect_timeout" };
            }
            _ = async { banner.as_mut().expect("guarded").closed().await }, if banner.is_some() => {
                tracing::info!(sid, "disconnected from the host's banner");
                break "host_disconnected";
            }
        };
        match event {
            None | Some(SessionEvent::RemoteEnd) => break "remote_end",
            Some(SessionEvent::Failed(reason)) => break reason,
            Some(SessionEvent::Answer(answer)) => pc
                .set_remote_description(*answer)
                .await
                .context("apply answer")?,
            Some(SessionEvent::Candidate(c)) => {
                if let Err(e) = pc.add_ice_candidate(c).await {
                    tracing::warn!(error = %e, "ignoring bad remote ICE candidate");
                }
            }
            Some(SessionEvent::Connected) | Some(SessionEvent::Authorized) => {
                match event {
                    Some(SessionEvent::Connected) => {
                        connected = true;
                        if !authorized {
                            // The viewer now has time to enter the PIN.
                            deadline
                                .as_mut()
                                .reset(tokio::time::Instant::now() + PIN_ENTRY_TIMEOUT);
                        }
                    }
                    _ => authorized = true,
                }
                if !(connected && authorized) {
                    continue;
                }
                if let Some(cap) = capturer.take() {
                    // The banner goes up before the first frame leaves; no banner, no session.
                    if let Some(text) = &cfg.banner {
                        match crate::ui::Banner::show(text) {
                            Ok(b) => banner = Some(b),
                            Err(e) => {
                                tracing::error!(error = %format!("{e:#}"), "cannot show the session banner");
                                break "banner_unavailable";
                            }
                        }
                    }
                    let payload_type = sender
                        .get_parameters()
                        .await?
                        .rtp_parameters
                        .codecs
                        .first()
                        .map(|c| c.payload_type)
                        .context("no negotiated codec")?;
                    let frames = spawn_pipeline(cap, media.clone(), ctl.clone());
                    streaming = Some(tokio::spawn(stream_frames(
                        Arc::clone(&track),
                        ssrc,
                        payload_type,
                        media.encoder.fps,
                        frames,
                    )));
                    tokio::spawn(forward_keyframe_requests(Arc::clone(&track), ctl.clone()));
                    tracing::info!(sid, "streaming started");
                }
            }
        }
    };

    ctl.stop.store(true, Ordering::Relaxed);
    if let Some(task) = streaming {
        task.abort();
    }
    control_task.abort(); // drops the injector → releases held keys/buttons
    drop(banner); // removes the banner
    let _ = pc.close().await;
    Ok(reason)
}

/// Writes encoded frames to the track, logging throughput every few seconds.
async fn stream_frames(
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    payload_type: u8,
    fps: u32,
    mut frames: mpsc::Receiver<crate::media::TimedFrame>,
) {
    let default_duration = Duration::from_secs(1) / fps.max(1);
    let mut prev: Option<Instant> = None;
    let (mut count, mut bytes, mut window) = (0u32, 0usize, Instant::now());
    while let Some(timed) = frames.recv().await {
        let duration = prev.map_or(default_duration, |p| {
            timed.captured_at.saturating_duration_since(p)
        });
        prev = Some(timed.captured_at);
        bytes += timed.frame.data.len();
        count += 1;
        let sample = Sample {
            data: Bytes::from(timed.frame.data),
            duration,
            ..Sample::new(timed.captured_at)
        };
        if let Err(e) = track
            .sample_writer(ssrc, payload_type)
            .write_sample(&sample)
            .await
        {
            tracing::warn!(error = %e, "write sample failed; stopping stream");
            break;
        }
        if window.elapsed() >= Duration::from_secs(5) {
            let secs = window.elapsed().as_secs_f64();
            tracing::info!(
                fps = format!("{:.1}", f64::from(count) / secs),
                kbps = (bytes as f64 * 8.0 / secs / 1000.0) as u64,
                size = format!("{}x{}", timed.frame.width, timed.frame.height),
                "stream stats"
            );
            (count, bytes, window) = (0, 0, Instant::now());
        }
    }
}

/// Turns PLI/FIR from the viewer into an immediate keyframe.
async fn forward_keyframe_requests(track: Arc<TrackLocalStaticSample>, ctl: PipelineControl) {
    while let Some(event) = track.poll().await {
        let TrackLocalEvent::OnRtcpPacket(packets) = event else {
            continue;
        };
        if packets.iter().any(|p| is_keyframe_request(p.as_ref())) {
            tracing::debug!("keyframe requested by viewer");
            ctl.keyframe.store(true, Ordering::Relaxed);
        }
        if ctl.stop.load(Ordering::Relaxed) {
            break;
        }
    }
}

fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    u32::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Source;
    use std::sync::atomic::AtomicUsize;
    use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};

    /// A webrtc-rs peer standing in for the browser.
    struct ViewerHandler {
        candidates: mpsc::UnboundedSender<RTCIceCandidateInit>,
        frames: Arc<AtomicUsize>,
        packets: Arc<AtomicUsize>,
        channels: mpsc::UnboundedSender<Arc<dyn webrtc::data_channel::DataChannel>>,
    }

    #[async_trait::async_trait]
    impl PeerConnectionEventHandler for ViewerHandler {
        async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
            if let Ok(c) = event.candidate.to_json() {
                let _ = self.candidates.send(c);
            }
        }

        async fn on_data_channel(&self, dc: Arc<dyn webrtc::data_channel::DataChannel>) {
            let _ = self.channels.send(dc);
        }

        async fn on_track(&self, track: Arc<dyn TrackRemote>) {
            let (frames, packets) = (Arc::clone(&self.frames), Arc::clone(&self.packets));
            tokio::spawn(async move {
                while let Some(ev) = track.poll().await {
                    if let TrackRemoteEvent::OnRtpPacket(p) = ev {
                        packets.fetch_add(1, Ordering::Relaxed);
                        if p.header.marker {
                            frames.fetch_add(1, Ordering::Relaxed); // last packet of a frame
                        }
                    }
                }
            });
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn streams_h264_to_a_webrtc_peer_over_real_ice() {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Envelope>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let recorded = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let media = MediaOptions {
            source: Source::TestPattern,
            input: crate::media::InputMode::Record(Arc::clone(&recorded)),
            ..Default::default()
        };
        let capturer = crate::media::open_source(&media).unwrap();
        let cfg = SessionConfig {
            sid: "ses_test".into(),
            ice_servers: vec![],
            media,
            gate: None,
            banner: None,
        };
        let session = tokio::spawn(run_session(cfg, capturer, out_tx, ev_tx.clone(), ev_rx));

        let (cand_tx, mut cand_rx) = mpsc::unbounded_channel();
        let (dc_tx, mut dc_rx) = mpsc::unbounded_channel();
        let frames = Arc::new(AtomicUsize::new(0));
        let packets = Arc::new(AtomicUsize::new(0));
        let viewer = build_peer(
            vec![],
            Arc::new(ViewerHandler {
                candidates: cand_tx,
                frames: Arc::clone(&frames),
                packets: Arc::clone(&packets),
                channels: dc_tx,
            }),
        )
        .await
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(20);
        while frames.load(Ordering::Relaxed) < 30 {
            assert!(
                Instant::now() < deadline,
                "only {} frames ({} packets) arrived",
                frames.load(Ordering::Relaxed),
                packets.load(Ordering::Relaxed)
            );
            tokio::select! {
                Some(env) = out_rx.recv() => match env.message_type() {
                    Some(MessageType::SdpOffer) => {
                        let offer: RTCSessionDescription = serde_json::from_value(env.body).unwrap();
                        assert!(offer.sdp.contains("H264"), "offer must advertise H.264");
                        viewer.set_remote_description(offer).await.unwrap();
                        let answer = viewer.create_answer(None).await.unwrap();
                        viewer.set_local_description(answer.clone()).await.unwrap();
                        ev_tx.send(SessionEvent::Answer(Box::new(answer))).unwrap();
                    }
                    Some(MessageType::IceCandidate) => {
                        let c: RTCIceCandidateInit = serde_json::from_value(env.body).unwrap();
                        viewer.add_ice_candidate(c).await.unwrap();
                    }
                    Some(MessageType::SessionEnd) => panic!("session ended early: {}", env.body),
                    other => panic!("unexpected {other:?}"),
                },
                Some(c) = cand_rx.recv() => { ev_tx.send(SessionEvent::Candidate(c)).unwrap(); }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }

        // Remote control: both channels arrive; ctl announces displays and input status.
        use carson_proto::input::{ControlMessage, InputMessage};
        use webrtc::data_channel::DataChannelEvent;
        let mut channels = std::collections::HashMap::new();
        while channels.len() < 2 {
            let dc = tokio::time::timeout(Duration::from_secs(5), dc_rx.recv())
                .await
                .expect("data channels")
                .unwrap();
            channels.insert(dc.label().await.unwrap(), dc);
        }
        let (ptr, ctl) = (&channels["ptr"], &channels["ctl"]);
        assert!(!ptr.ordered().await.unwrap(), "ptr must be unordered");
        // webrtc-rs reports max_retransmits=None for remotely opened channels regardless of the
        // DCEP reliability parameter; the browser check (RTCDataChannel.maxRetransmits) covers it.
        assert!(
            ctl.ordered().await.unwrap(),
            "ctl must be reliable and ordered"
        );

        let mut control = Vec::new();
        while control.len() < 2 {
            let ev = tokio::time::timeout(Duration::from_secs(5), ctl.poll())
                .await
                .expect("control message")
                .unwrap();
            if let DataChannelEvent::OnMessage(m) = ev {
                assert!(m.is_string);
                control.push(serde_json::from_slice::<ControlMessage>(&m.data).unwrap());
            }
        }
        assert!(
            matches!(&control[0], ControlMessage::Displays { displays, current: 0 } if displays.len() == 1 && displays[0].width == 1280)
        );
        assert_eq!(
            control[1],
            ControlMessage::InputStatus {
                available: true,
                message: String::new()
            }
        );

        let send = |dc: &Arc<dyn webrtc::data_channel::DataChannel>, m: InputMessage| {
            let dc = Arc::clone(dc);
            async move {
                dc.send(bytes::BytesMut::from(&m.encode()[..]))
                    .await
                    .unwrap()
            }
        };
        send(ptr, InputMessage::PointerMove { x: 65535, y: 0 }).await;
        send(
            ctl,
            InputMessage::Key {
                down: true,
                mods: 0,
                code: "KeyA".into(),
            },
        )
        .await;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let ev = recorded.lock().unwrap().clone();
            if ev.iter().any(|e| e == "key KeyA down")
                && ev.iter().any(|e| e == "move 1279,0 held=0")
            {
                break;
            }
            assert!(Instant::now() < deadline, "input not injected: {ev:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Display switching: the agent reopens the source and re-announces the displays.
        ctl.send_text(r#"{"type":"select_display","index":0}"#)
            .await
            .unwrap();
        let frames_before = frames.load(Ordering::Relaxed);
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(5), ctl.poll())
                .await
                .expect("displays reply")
                .unwrap();
            if let DataChannelEvent::OnMessage(m) = ev
                && m.is_string
                && let Ok(ControlMessage::Displays { current, .. }) =
                    serde_json::from_slice(&m.data)
            {
                assert_eq!(current, 0);
                break;
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while frames.load(Ordering::Relaxed) < frames_before + 10 {
            assert!(
                Instant::now() < deadline,
                "stream stalled after display switch"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // An out-of-range index is ignored (no reply, stream keeps going).
        ctl.send_text(r#"{"type":"select_display","index":7}"#)
            .await
            .unwrap();

        // A viewer hanging up ends the session cleanly, without a session.end echo.
        ev_tx.send(SessionEvent::RemoteEnd).unwrap();
        tokio::time::timeout(Duration::from_secs(5), session)
            .await
            .expect("session did not stop")
            .unwrap();
        while let Ok(env) = out_rx.try_recv() {
            assert_ne!(
                env.message_type(),
                Some(MessageType::SessionEnd),
                "no end echo on remote end"
            );
        }
        let _ = viewer.close().await;

        // The held key is released when the session goes away.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !recorded.lock().unwrap().iter().any(|e| e == "key KeyA up") {
            assert!(
                Instant::now() < deadline,
                "held key never released: {:?}",
                recorded.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Relays signaling between a session and the test viewer until `done()` holds.
    async fn relay_until(
        out_rx: &mut mpsc::UnboundedReceiver<Envelope>,
        cand_rx: &mut mpsc::UnboundedReceiver<RTCIceCandidateInit>,
        viewer: &Arc<dyn PeerConnection>,
        ev_tx: &mpsc::UnboundedSender<SessionEvent>,
        mut done: impl FnMut() -> bool,
        what: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::select! {
                Some(env) = out_rx.recv() => match env.message_type() {
                    Some(MessageType::SdpOffer) => {
                        let offer: RTCSessionDescription = serde_json::from_value(env.body).unwrap();
                        viewer.set_remote_description(offer).await.unwrap();
                        let answer = viewer.create_answer(None).await.unwrap();
                        viewer.set_local_description(answer.clone()).await.unwrap();
                        ev_tx.send(SessionEvent::Answer(Box::new(answer))).unwrap();
                    }
                    Some(MessageType::IceCandidate) => {
                        viewer.add_ice_candidate(serde_json::from_value(env.body).unwrap()).await.unwrap();
                    }
                    Some(MessageType::SessionEnd) => panic!("session ended while waiting for {what}: {}", env.body),
                    other => panic!("unexpected {other:?}"),
                },
                Some(c) = cand_rx.recv() => { ev_tx.send(SessionEvent::Candidate(c)).unwrap(); }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
    }

    /// Next JSON control message on `ctl`.
    async fn next_control(
        ctl: &Arc<dyn webrtc::data_channel::DataChannel>,
    ) -> carson_proto::input::ControlMessage {
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(5), ctl.poll())
                .await
                .expect("control message")
                .unwrap();
            if let webrtc::data_channel::DataChannelEvent::OnMessage(m) = ev
                && m.is_string
            {
                return serde_json::from_slice(&m.data).unwrap();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unattended_pin_gates_video_and_input() {
        use carson_proto::input::{ControlMessage, InputMessage};
        let recorded = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let media = MediaOptions {
            source: Source::TestPattern,
            input: crate::media::InputMode::Record(Arc::clone(&recorded)),
            ..Default::default()
        };
        let gate = crate::control::PinGate {
            hash: crate::consent::hash_pin("482916").unwrap(),
            guard: Arc::default(),
        };
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Envelope>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let capturer = crate::media::open_source(&media).unwrap();
        let cfg = SessionConfig {
            sid: "ses_pin".into(),
            ice_servers: vec![],
            media,
            gate: Some(gate),
            banner: None,
        };
        let session = tokio::spawn(run_session(cfg, capturer, out_tx, ev_tx.clone(), ev_rx));

        let (cand_tx, mut cand_rx) = mpsc::unbounded_channel();
        let (dc_tx, mut dc_rx) = mpsc::unbounded_channel();
        let frames = Arc::new(AtomicUsize::new(0));
        let viewer = build_peer(
            vec![],
            Arc::new(ViewerHandler {
                candidates: cand_tx,
                frames: Arc::clone(&frames),
                packets: Arc::default(),
                channels: dc_tx,
            }),
        )
        .await
        .unwrap();

        let channels = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        {
            let channels = Arc::clone(&channels);
            tokio::spawn(async move {
                while let Some(dc) = dc_rx.recv().await {
                    let label = dc.label().await.unwrap();
                    channels.lock().unwrap().insert(label, dc);
                }
            });
        }
        relay_until(
            &mut out_rx,
            &mut cand_rx,
            &viewer,
            &ev_tx,
            || channels.lock().unwrap().len() == 2,
            "data channels",
        )
        .await;
        let ctl = channels.lock().unwrap()["ctl"].clone();

        assert!(matches!(
            next_control(&ctl).await,
            ControlMessage::Displays { .. }
        ));
        assert_eq!(
            next_control(&ctl).await,
            ControlMessage::PinRequired { attempts_left: 5 }
        );

        // Input before the PIN is ignored, and no video flows.
        ctl.send(bytes::BytesMut::from(
            &InputMessage::Key {
                down: true,
                mods: 0,
                code: "KeyX".into(),
            }
            .encode()[..],
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            frames.load(Ordering::Relaxed),
            0,
            "video must not flow before the PIN"
        );
        assert!(
            recorded.lock().unwrap().is_empty(),
            "input must not be injected before the PIN"
        );

        ctl.send_text(r#"{"type":"pin","pin":"000000"}"#)
            .await
            .unwrap();
        assert_eq!(
            next_control(&ctl).await,
            ControlMessage::PinResult {
                ok: false,
                attempts_left: 4,
                retry_after_s: 0
            }
        );
        ctl.send_text(r#"{"type":"pin","pin":"482916"}"#)
            .await
            .unwrap();
        assert!(matches!(
            next_control(&ctl).await,
            ControlMessage::PinResult { ok: true, .. }
        ));
        assert_eq!(
            next_control(&ctl).await,
            ControlMessage::InputStatus {
                available: true,
                message: String::new()
            }
        );

        relay_until(
            &mut out_rx,
            &mut cand_rx,
            &viewer,
            &ev_tx,
            || frames.load(Ordering::Relaxed) >= 10,
            "video after PIN",
        )
        .await;
        ctl.send(bytes::BytesMut::from(
            &InputMessage::Key {
                down: true,
                mods: 0,
                code: "KeyY".into(),
            }
            .encode()[..],
        ))
        .await
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !recorded
            .lock()
            .unwrap()
            .iter()
            .any(|e| e == "key KeyY down")
        {
            assert!(
                Instant::now() < deadline,
                "input not injected after PIN: {:?}",
                recorded.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !recorded.lock().unwrap().iter().any(|e| e.contains("KeyX")),
            "pre-PIN input leaked"
        );

        ev_tx.send(SessionEvent::RemoteEnd).unwrap();
        tokio::time::timeout(Duration::from_secs(5), session)
            .await
            .expect("session did not stop")
            .unwrap();
        let _ = viewer.close().await;
    }
}
