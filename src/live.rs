//! In-process fan-out for the live play-session check-in SSE stream
//! (`13-checkins.md` §5) — the one place in this app that needs
//! genuinely real-time push to an open browser tab, distinct from Web
//! Push (`notify.rs`), which targets a device that may not have the
//! app open. Nothing here is persisted: a dropped connection simply
//! misses events until it reconnects, which is fine since the
//! underlying check-in data is always available via the ordinary
//! REST `GET`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 32;

#[derive(Clone)]
pub enum StreamEvent {
    /// A check-in belonging to this play session was created or
    /// updated; the payload is the same JSON body the ordinary REST
    /// response for that check-in would return.
    CheckinUpdated(String),
    /// The session left `in_progress` (ended or cancelled) — the last
    /// event any subscriber sees; `checkin_stream` forwards it and
    /// then closes the connection, since nothing is "live" anymore.
    SessionEnded,
}

/// Keyed by `play_sessions.id`. Channels are created lazily on first
/// subscribe and reaped lazily on the next publish that finds no
/// receivers left, so this never needs an explicit teardown hook.
#[derive(Clone, Default)]
pub struct PlaySessionStreams {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<StreamEvent>>>>,
}

impl PlaySessionStreams {
    pub fn subscribe(&self, session_id: &str) -> broadcast::Receiver<StreamEvent> {
        let mut channels = self.channels.lock().unwrap();
        channels
            .entry(session_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe()
    }

    /// Cheap no-op when nobody is subscribed — the checkins/play-session
    /// API handlers call this unconditionally on every relevant write,
    /// so it must stay side-effect-free in the common case where no
    /// one is watching a live session right now.
    fn publish(&self, session_id: &str, event: StreamEvent) {
        let mut channels = self.channels.lock().unwrap();
        let Some(sender) = channels.get(session_id) else {
            return;
        };
        if sender.send(event).is_err() {
            channels.remove(session_id);
        }
    }

    pub fn publish_checkin(&self, session_id: &str, checkin_json: String) {
        self.publish(session_id, StreamEvent::CheckinUpdated(checkin_json));
    }

    pub fn publish_session_ended(&self, session_id: &str) {
        self.publish(session_id, StreamEvent::SessionEnded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_with_no_subscriber_is_a_harmless_no_op() {
        let streams = PlaySessionStreams::default();
        // No panic, no channel silently created for nobody to read.
        streams.publish_checkin("nobody-is-watching", "{}".to_string());
        streams.publish_session_ended("nobody-is-watching");
    }

    #[tokio::test]
    async fn a_subscriber_receives_checkin_and_session_ended_events_in_order() {
        let streams = PlaySessionStreams::default();
        let mut rx = streams.subscribe("session-1");

        streams.publish_checkin("session-1", "payload-a".to_string());
        streams.publish_session_ended("session-1");

        match rx.recv().await.unwrap() {
            StreamEvent::CheckinUpdated(payload) => assert_eq!(payload, "payload-a"),
            StreamEvent::SessionEnded => panic!("expected the checkin event first"),
        }
        assert!(matches!(
            rx.recv().await.unwrap(),
            StreamEvent::SessionEnded
        ));
    }

    #[tokio::test]
    async fn events_for_one_session_never_reach_a_subscriber_of_another() {
        let streams = PlaySessionStreams::default();
        let mut rx_a = streams.subscribe("session-a");
        let mut rx_b = streams.subscribe("session-b");

        streams.publish_checkin("session-a", "only-for-a".to_string());

        match rx_a.recv().await.unwrap() {
            StreamEvent::CheckinUpdated(payload) => assert_eq!(payload, "only-for-a"),
            StreamEvent::SessionEnded => panic!("unexpected session_ended"),
        }
        // session-b's receiver has nothing waiting for it.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), rx_b.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_fresh_subscribe_after_every_receiver_dropped_does_not_see_stale_events() {
        let streams = PlaySessionStreams::default();
        {
            let _rx = streams.subscribe("session-1");
            streams.publish_checkin("session-1", "before-reconnect".to_string());
            // `_rx` drops here — the only receiver.
        }
        // The next publish attempt (from any handler) finds no
        // receivers and reaps the dead channel.
        streams.publish_checkin("session-1", "also-before-reconnect".to_string());

        let mut rx = streams.subscribe("session-1");
        streams.publish_checkin("session-1", "after-reconnect".to_string());
        match rx.recv().await.unwrap() {
            StreamEvent::CheckinUpdated(payload) => assert_eq!(payload, "after-reconnect"),
            StreamEvent::SessionEnded => panic!("unexpected session_ended"),
        }
    }
}
