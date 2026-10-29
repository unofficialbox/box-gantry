//! Box's long-poll event stream: [`EventStream`] follows the protocol over the
//! typed calls an [`EventSource`] (a generated events manager) supplies.

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::{Client, Error};

/// One of Box's real-time servers, as `OPTIONS /events` lists them. `None`
/// takes Box's documented default.
#[derive(Clone, Debug)]
pub struct LongPollServer {
    pub url: String,
    pub max_retries: Option<u32>,
    pub retry_timeout: Option<Duration>,
}

/// What [`EventStream`] drives: the typed calls a generated events manager
/// supplies.
pub trait EventSource {
    type Event;
    /// List a real-time server to long-poll (`None` when Box lists none).
    fn server(&self) -> impl Future<Output = Result<Option<LongPollServer>, Error>> + Send;
    /// The events after a stream position, and the position to wait from next.
    fn page(
        &self,
        position: &str,
    ) -> impl Future<Output = Result<(Vec<Self::Event>, Option<String>), Error>> + Send;
    /// The event's id, so a repeat can be dropped.
    fn id(event: &Self::Event) -> Option<&str>;
}

const DEFAULT_LONG_POLL_RETRIES: u32 = 10;
const DEFAULT_LONG_POLL_TIMEOUT: Duration = Duration::from_secs(610);
/// Box may deliver an event twice; this many recent event ids are kept to drop
/// the repeats (the bound Box's own SDKs use).
const MAX_SEEN_EVENTS: usize = 1000;

/// Box's long-poll event stream from a position (`"now"` for events from here
/// on): [`EventStream::next`] yields the events after it, each once, then waits
/// on a real-time server until Box reports a change and yields those. A
/// `reconnect` answer, or exhausting the server's retries, lists a fresh
/// server. A long poll that times out or fails in transit is retried; an error
/// response from Box is returned once, and the stream then ends. Drop the
/// stream (or the `next` future) to stop it.
pub struct EventStream<S: EventSource> {
    client: Arc<Client>,
    source: S,
    position: String,
    pending: VecDeque<S::Event>,
    seen: HashSet<String>,
    server: Option<(LongPollServer, u32)>,
    fetched: bool,
    done: bool,
}

impl<S: EventSource> EventStream<S> {
    pub fn new(client: Arc<Client>, position: impl Into<String>, source: S) -> Self {
        EventStream {
            client,
            source,
            position: position.into(),
            pending: VecDeque::new(),
            seen: HashSet::new(),
            server: None,
            fetched: false,
            done: false,
        }
    }

    /// The next event; `None` once an error has ended the stream.
    pub async fn next(&mut self) -> Option<Result<S::Event, Error>> {
        if self.done {
            return None;
        }
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.fetched {
                if let Err(err) = self.wait_for_change().await {
                    self.done = true;
                    return Some(Err(err));
                }
            }
            if let Err(err) = self.fetch().await {
                self.done = true;
                return Some(Err(err));
            }
        }
    }

    /// Fetch the events after the position, queue the unseen ones, advance.
    async fn fetch(&mut self) -> Result<(), Error> {
        let (events, next) = self.source.page(&self.position).await?;
        self.fetched = true;
        let ids: Vec<String> = events
            .iter()
            .filter_map(|event| S::id(event).map(str::to_string))
            .collect();
        for event in events {
            if let Some(id) = S::id(&event) {
                if !self.seen.insert(id.to_string()) {
                    continue;
                }
            }
            self.pending.push_back(event);
        }
        if self.seen.len() >= MAX_SEEN_EVENTS {
            // Repeats arrive close together: keep only this page's ids.
            self.seen = ids.into_iter().collect();
        }
        if let Some(next) = next.filter(|next| !next.is_empty()) {
            self.position = next;
        }
        Ok(())
    }

    /// Long-poll until Box reports a change after the position.
    async fn wait_for_change(&mut self) -> Result<(), Error> {
        loop {
            let exhausted = self
                .server
                .as_ref()
                .is_none_or(|(server, retries)| *retries >= server.max_retries.unwrap_or(0));
            if exhausted {
                let mut listed = self
                    .source
                    .server()
                    .await?
                    .ok_or_else(|| Error::new("box: no real-time server to long-poll"))?;
                listed.max_retries = Some(
                    listed
                        .max_retries
                        .filter(|n| *n > 0)
                        .unwrap_or(DEFAULT_LONG_POLL_RETRIES),
                );
                self.server = Some((listed, 0));
            }
            let (server, retries) = self.server.as_mut().expect("listed above");
            *retries += 1;
            match long_poll(&self.client, server, &self.position).await {
                Ok(message) if message == "new_change" => return Ok(()),
                Ok(message) if message == "reconnect" || message == "max_retries" => {
                    self.server = None;
                }
                Ok(_) => {}
                // An error response from Box ends the stream.
                Err(err) if err.status().is_some() => return Err(err),
                // Timed out or failed in transit: poll again.
                Err(_) => {}
            }
        }
    }
}

/// Wait on a real-time server for Box's next message about the stream after
/// `position`: `new_change`, `reconnect`, or `max_retries`.
async fn long_poll(
    client: &Client,
    server: &LongPollServer,
    position: &str,
) -> Result<String, Error> {
    let request = crate::with_query(
        client.new_request("GET", &server.url),
        "stream_position",
        position,
    );
    let timeout = server
        .retry_timeout
        .filter(|t| !t.is_zero())
        .unwrap_or(DEFAULT_LONG_POLL_TIMEOUT);
    let response = client.fetch(crate::with_timeout(request, timeout)).await?;
    let answer: serde_json::Value = serde_json::from_slice(&crate::response_bytes(&response)?)?;
    Ok(answer["message"].as_str().unwrap_or_default().to_string())
}

/// A stream position Box sent as a JSON string or number, as the string to send
/// back.
pub fn stream_position(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Auth;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A real-time server answering long polls from a script, one answer per
    /// connection; `hang` holds the request past the poll's timeout. Records
    /// the stream position each poll waited from.
    async fn realtime(answers: Vec<&'static str>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/subscribe?channel=c",
            listener.local_addr().unwrap()
        );
        let positions = Arc::new(Mutex::new(Vec::new()));
        let seen = positions.clone();
        tokio::spawn(async move {
            let mut answers = VecDeque::from(answers);
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let position = head
                    .split("stream_position=")
                    .nth(1)
                    .and_then(|rest| rest.split([' ', '&']).next())
                    .unwrap_or_default()
                    .to_string();
                seen.lock().unwrap().push(position);
                let answer = answers.pop_front().unwrap_or("reconnect");
                let (status, body) = match answer {
                    "hang" => {
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            drop(socket);
                        });
                        continue;
                    }
                    "error" => (
                        "400 Bad Request",
                        r#"{"type":"error","status":400,"code":"bad_request"}"#.to_string(),
                    ),
                    message => (
                        "200 OK",
                        format!(r#"{{"version":1,"message":"{message}"}}"#),
                    ),
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        (url, positions)
    }

    /// Scripts `page` (the events per call; the next position is the call
    /// number) and counts the servers listed.
    struct Scripted {
        url: String,
        pages: Mutex<VecDeque<Vec<&'static str>>>,
        calls: Mutex<usize>,
        servers: Mutex<usize>,
        timeout: Duration,
    }

    impl EventSource for Scripted {
        type Event = &'static str;

        async fn server(&self) -> Result<Option<LongPollServer>, Error> {
            *self.servers.lock().unwrap() += 1;
            Ok(Some(LongPollServer {
                url: self.url.clone(),
                max_retries: None,
                retry_timeout: Some(self.timeout),
            }))
        }

        async fn page(&self, position: &str) -> Result<(Vec<&'static str>, Option<String>), Error> {
            let mut calls = self.calls.lock().unwrap();
            let expected = if *calls == 0 {
                "now".to_string()
            } else {
                calls.to_string()
            };
            assert_eq!(position, expected, "page {calls}'s position");
            *calls += 1;
            let events = self.pages.lock().unwrap().pop_front().unwrap_or_default();
            Ok((events, Some(calls.to_string())))
        }

        fn id<'a>(event: &'a &'static str) -> Option<&'a str> {
            Some(event.trim_end_matches('\''))
        }
    }

    fn scripted(url: String, pages: Vec<Vec<&'static str>>, timeout: Duration) -> Scripted {
        Scripted {
            url,
            pages: Mutex::new(pages.into()),
            calls: Mutex::new(0),
            servers: Mutex::new(0),
            timeout,
        }
    }

    fn client() -> Arc<Client> {
        Arc::new(Client::new(Auth::developer_token("t")).with_max_retries(0))
    }

    #[tokio::test]
    async fn the_stream_follows_the_long_poll_protocol_and_drops_repeats() {
        let (url, positions) =
            realtime(vec!["reconnect", "new_change", "hang", "new_change"]).await;
        // "now" yields nothing; then a and b; then b again (a repeat) with c.
        let source = scripted(
            url,
            vec![vec![], vec!["a", "b"], vec!["b'", "c"]],
            Duration::from_millis(200),
        );
        let mut stream = EventStream::new(client(), "now", source);
        let mut got = Vec::new();
        while got.len() < 3 {
            got.push(stream.next().await.unwrap().unwrap());
        }
        assert_eq!(got, ["a", "b", "c"]);
        assert_eq!(
            *stream.source.servers.lock().unwrap(),
            2,
            "a reconnect lists a fresh server"
        );
        assert_eq!(
            *positions.lock().unwrap(),
            ["1", "1", "2", "2"],
            "a timed-out poll is retried"
        );
    }

    #[tokio::test]
    async fn an_error_response_ends_the_stream_and_dropping_it_stops_it() {
        let (url, _) = realtime(vec!["error"]).await;
        let mut stream = EventStream::new(
            client(),
            "now",
            scripted(url, vec![], Duration::from_secs(1)),
        );
        let err = stream.next().await.unwrap().unwrap_err();
        assert_eq!(err.status(), Some(400));
        assert!(stream.next().await.is_none(), "the stream ended");

        let (url, _) = realtime(vec!["hang"]).await;
        let mut stream = EventStream::new(
            client(),
            "now",
            scripted(url, vec![], Duration::from_secs(5)),
        );
        let waited = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
        assert!(
            waited.is_err(),
            "the long poll was still waiting when dropped"
        );
    }

    #[test]
    fn stream_positions_round_trip_exactly() {
        let page: serde_json::Value =
            serde_json::from_str(r#"{"next_stream_position":1152922976252290886}"#).unwrap();
        assert_eq!(
            stream_position(&page["next_stream_position"]).as_deref(),
            Some("1152922976252290886")
        );
        assert_eq!(
            stream_position(&serde_json::json!("42")).as_deref(),
            Some("42")
        );
    }
}
