//! Embeddings from an API, for machines that should not run a model.
//!
//! [`crate::embed::LocalEmbedder`] is the default and stays the default: it
//! costs a download and some CPU, and it asks nobody for a key. But it is not
//! free of consequences — the model is 88 MB, inference wants a core, and on a
//! small server that is the difference between memory being cheap to run and
//! memory being the reason the box is busy. This is the other end of that
//! trade: a request per embedding, someone else's hardware, and a key.
//!
//! One shape, not one vendor. `/v1/embeddings` with `{model, input}` and
//! `{data: [{embedding: [...]}]}` back is what OpenAI defined and what
//! everything compatible with it accepts, which is the same reason
//! `crate::openai` exists rather than a provider per company.
//!
//! **The model name is not decoration.** Two embedding models put vectors in
//! unrelated spaces, and cosine similarity between them is a number with no
//! meaning rather than an error. Every vector is stored beside the name of
//! what produced it, so switching from the local model to a hosted one does
//! not corrupt anything — it leaves the old vectors uncomparable and
//! unconsulted until `anamnesis reindex` writes new ones.

use std::time::Duration;

use anamnesis_core::embedding::Embed;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

use crate::embed::{EmbedError, Embedder};

/// Where an OpenAI-compatible endpoint lives when nobody says.
pub const DEFAULT_URL: &str = "https://api.openai.com/v1/embeddings";

/// The model asked for when nobody names one.
pub const DEFAULT_MODEL: &str = "text-embedding-3-small";

/// How long one embedding call may take.
///
/// An embedding happens while a page is being written or a query answered, and
/// both have somebody waiting. The vector stream is the one part of retrieval
/// that is allowed to be missing, so a slow endpoint costs a page its place in
/// that stream rather than costing the write.
const TIMEOUT: Duration = Duration::from_secs(10);

/// How long a connection to an endpoint on this machine may take to open.
///
/// A loopback port that is listening accepts in well under a millisecond. One
/// that is not is refused at once on Linux and macOS, and on Windows only after
/// the connection is tried again for two seconds — measured on the machine
/// this project runs on, with Ollama stopped: 2.06 s, twice in a row. Every
/// page write and every query while Ollama is down paid that.
const LOOPBACK_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// Whether an endpoint's host is this machine.
fn is_loopback(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// An embeddings endpoint that speaks OpenAI's shape.
pub struct HostedEmbedder {
    client: reqwest::blocking::Client,
    url: String,
    model: String,
    key: Option<SecretString>,
    dimension: usize,
}

impl HostedEmbedder {
    /// Connect, and learn the dimension by asking once.
    ///
    /// The probe is the point. A hosted embedder cannot know its own vector
    /// length without asking, and asking at startup means a wrong key, a wrong
    /// URL or a model that does not exist is an error somebody sees while
    /// starting the server — not hours later, in a log, after sessions have
    /// been summarised without a vector each.
    pub fn connect(
        url: impl Into<String>,
        model: impl Into<String>,
        key: Option<SecretString>,
    ) -> Result<Self, EmbedError> {
        let url = url.into();
        let model = model.into();
        let mut client = reqwest::blocking::Client::builder().timeout(TIMEOUT);
        if is_loopback(&url) {
            client = client.connect_timeout(LOOPBACK_CONNECT_TIMEOUT);
        }
        let client = client.build().map_err(|error| EmbedError::Fetch {
            model: model.clone(),
            reason: error.to_string(),
        })?;

        let mut embedder = Self {
            client,
            url,
            model,
            key,
            dimension: 0,
        };
        embedder.dimension = embedder
            .request("dimension probe")
            .map_err(|reason| EmbedError::Load {
                model: embedder.model.clone(),
                reason,
            })?
            .len();
        Ok(embedder)
    }

    /// One embedding, or a sentence saying why not.
    fn request(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut post = self.client.post(&self.url).json(&body(&self.model, text));
        if let Some(key) = &self.key {
            post = post.bearer_auth(key.expose_secret());
        }

        let response = post.send().map_err(|error| error.to_string())?;
        let status = response.status();
        let text = response.text().map_err(|error| error.to_string())?;
        if !status.is_success() {
            // Through the same classifier the completion providers use, so a
            // 429 reads the same way whichever half of this crate met it.
            return Err(crate::http::api_error(status.as_u16(), &text, None).to_string());
        }

        let payload: Value = serde_json::from_str(&text)
            .map_err(|error| format!("the endpoint did not answer with JSON: {error}"))?;
        vector(&payload)
    }
}

impl Embed for HostedEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.request(text)
    }
}

impl Embedder for HostedEmbedder {
    fn dimension(&self) -> usize {
        self.dimension
    }
}

/// How long a [`Reconnecting`] embedder waits after a failed connection before
/// asking the endpoint again.
///
/// Long enough that a query arriving every few seconds while the endpoint is
/// down does not spend its ten-second timeout each time, and short enough that
/// an endpoint started a minute after the agent is in use a minute later.
pub const RECONNECT_AFTER: Duration = Duration::from_secs(30);

/// A hosted embedder that connects when it is first needed, and again after a
/// failure.
///
/// For a process whose startup must not depend on the endpoint, which is every
/// process that has one. The MCP server was first: a harness starts it with the
/// agent, and a refusal there takes every memory tool away for the whole
/// session over the one retrieval stream that was allowed to be missing. `serve`
/// used to refuse, on the grounds that a scheduled task starts it and a refusal
/// is written where somebody looks — and after a reboot, with the Ollama beside
/// it not yet up, that refusal was written nowhere at all, and capture stopped
/// until somebody noticed.
///
/// So this starts unconnected. A query that finds it unconnected tries once;
/// a failure costs that query its vector stream, as a failed embedding always
/// has, and the next attempt waits [`RECONNECT_AFTER`]. Once connected it is
/// the embedder it wraps.
pub struct Reconnecting {
    url: String,
    model: String,
    key: Option<SecretString>,
    retry_after: Duration,
    state: std::sync::Mutex<Connection>,
}

/// Where a [`Reconnecting`] embedder stands.
enum Connection {
    /// Not connected, and not tried since this instant, if ever.
    Waiting(Option<std::time::Instant>),
    /// Connected.
    Ready(std::sync::Arc<HostedEmbedder>),
}

impl Reconnecting {
    /// An embedder for `url` and `model` that has not yet asked the endpoint
    /// anything.
    pub fn new(
        url: impl Into<String>,
        model: impl Into<String>,
        key: Option<SecretString>,
    ) -> Self {
        Self::retrying_after(url, model, key, RECONNECT_AFTER)
    }

    /// The same, with its own wait between attempts.
    pub fn retrying_after(
        url: impl Into<String>,
        model: impl Into<String>,
        key: Option<SecretString>,
        retry_after: Duration,
    ) -> Self {
        Self {
            url: url.into(),
            model: model.into(),
            key,
            retry_after,
            state: std::sync::Mutex::new(Connection::Waiting(None)),
        }
    }

    /// An embedder for an endpoint that did not answer a moment ago.
    ///
    /// For the caller that has just tried [`HostedEmbedder::connect`] and is
    /// falling back to this. Built with [`Reconnecting::new`], the first query
    /// asked the endpoint again at once — and on Windows a connection to a
    /// loopback port nothing listens on takes two seconds to be refused, so
    /// `anamnesis search` with Ollama down spent four seconds on two refusals
    /// of the same question. The attempt it was built after counts as the
    /// last one.
    pub fn after_a_failed_attempt(
        url: impl Into<String>,
        model: impl Into<String>,
        key: Option<SecretString>,
    ) -> Self {
        let embedder = Self::new(url, model, key);
        if let Ok(mut state) = embedder.state.lock() {
            *state = Connection::Waiting(Some(std::time::Instant::now()));
        }
        embedder
    }

    /// The connected embedder, connecting first if it is time to try.
    fn connected(&self) -> Result<std::sync::Arc<HostedEmbedder>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "the embedder's connection state was poisoned".to_owned())?;
        match &*state {
            Connection::Ready(embedder) => return Ok(embedder.clone()),
            Connection::Waiting(Some(tried)) if tried.elapsed() < self.retry_after => {
                return Err(format!(
                    "{} did not answer {}s ago; not asked again yet",
                    self.url,
                    tried.elapsed().as_secs()
                ));
            }
            Connection::Waiting(_) => {}
        }
        match HostedEmbedder::connect(&self.url, &self.model, self.key.clone()) {
            Ok(embedder) => {
                let embedder = std::sync::Arc::new(embedder);
                *state = Connection::Ready(embedder.clone());
                Ok(embedder)
            }
            Err(error) => {
                *state = Connection::Waiting(Some(std::time::Instant::now()));
                Err(error.to_string())
            }
        }
    }
}

impl Embed for Reconnecting {
    fn model(&self) -> &str {
        &self.model
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.connected()?.embed(text)
    }
}

impl Embedder for Reconnecting {
    /// The dimension once connected, and zero before: nothing outside tests
    /// reads it, and a vector length invented before the endpoint has answered
    /// would be a claim nobody was in a position to make.
    fn dimension(&self) -> usize {
        match self.state.lock().as_deref() {
            Ok(Connection::Ready(embedder)) => embedder.dimension(),
            _ => 0,
        }
    }
}

/// The request body every OpenAI-compatible endpoint takes.
///
/// One string rather than an array: the callers here embed a page or a query,
/// one at a time, and a batch API would be a second shape to keep working for
/// a saving nobody has measured.
fn body(model: &str, text: &str) -> Value {
    json!({ "model": model, "input": text })
}

/// The vector out of a response, or what was wrong with it.
fn vector(payload: &Value) -> Result<Vec<f32>, String> {
    let embedding = payload
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| data.first())
        .and_then(|first| first.get("embedding"))
        .and_then(Value::as_array)
        .ok_or_else(|| "the answer had no embedding in it".to_owned())?;

    if embedding.is_empty() {
        return Err("the answer carried an empty vector".to_owned());
    }

    embedding
        .iter()
        .map(|value| {
            value
                .as_f64()
                .map(|number| number as f32)
                .ok_or_else(|| "the vector had something in it that is not a number".to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn the_body_is_the_shape_every_compatible_endpoint_takes() {
        let sent = body("text-embedding-3-small", "why sqlite");

        assert_eq!(sent["model"], "text-embedding-3-small");
        assert_eq!(sent["input"], "why sqlite");
    }

    #[test]
    fn a_vector_is_read_out_of_the_answer() {
        let answered = json!({"data": [{"embedding": [0.5, -0.25, 1.0]}]});

        assert_eq!(vector(&answered).expect("vector"), vec![0.5, -0.25, 1.0]);
    }

    /// Three ways an answer can be shaped wrongly, and none of them may come
    /// back as a vector: a page filed with a broken embedding is a page the
    /// vector stream ranks by nonsense rather than one it skips.
    #[test]
    fn an_answer_that_is_not_a_vector_is_refused() {
        assert!(vector(&json!({"data": []})).is_err());
        assert!(vector(&json!({"data": [{"embedding": []}]})).is_err());
        assert!(vector(&json!({"data": [{"embedding": ["nope"]}]})).is_err());
    }

    /// The whole round trip against a socket that answers the way an endpoint
    /// does — the request line, the header, the body, and the vector back.
    /// The shape of the wire is the one thing unit tests on `body` and
    /// `vector` cannot check between them.
    #[test]
    fn it_speaks_to_something_that_answers_like_an_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");

        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut buffer = [0_u8; 4096];
            let read = socket.read(&mut buffer).expect("read");
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();

            let answer = json!({"data": [{"embedding": [0.1, 0.2, 0.3, 0.4]}]}).to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                )
                .expect("write");
            request
        });

        let embedder = HostedEmbedder::connect(
            format!("http://{address}/v1/embeddings"),
            "text-embedding-3-small",
            Some(SecretString::from("anam_test_key")),
        );

        let request = server.join().expect("server thread");
        assert!(request.starts_with("POST /v1/embeddings"), "{request}");
        assert!(
            request.contains("authorization: Bearer anam_test_key")
                || request.contains("Authorization: Bearer anam_test_key"),
            "the key was not presented: {request}"
        );
        assert!(request.contains("text-embedding-3-small"), "{request}");

        // The connection carried one probe, so the embedder learned its
        // dimension from it and the socket is closed; that is as far as one
        // accept can take this.
        let embedder = embedder.expect("connected");
        assert_eq!(embedder.dimension(), 4);
        assert_eq!(embedder.model(), "text-embedding-3-small");
    }

    /// A refusal at startup rather than a warning hours later: this is the
    /// whole reason the dimension is probed when the embedder is built.
    #[test]
    fn an_endpoint_that_refuses_is_a_startup_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");

        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut buffer = [0_u8; 4096];
            let _ = socket.read(&mut buffer);
            let answer =
                json!({"error": {"type": "invalid_api_key", "message": "bad key"}}).to_string();
            let _ = socket.write_all(
                format!(
                    "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
        });

        let refused = HostedEmbedder::connect(
            format!("http://{address}/v1/embeddings"),
            "text-embedding-3-small",
            None,
        );

        // Not `expect_err`: the success side is an embedder, and an embedder
        // is not something to require a `Debug` for so a test can print it.
        let Err(error) = refused else {
            panic!("an endpoint that refuses the key produced an embedder");
        };
        assert!(error.to_string().contains("bad key"), "{error}");
    }

    /// Answer one connection with `status` and a four-number vector.
    fn answer(socket: &mut std::net::TcpStream, status: &str) {
        let mut buffer = [0_u8; 4096];
        let _ = socket.read(&mut buffer);
        let body = if status.starts_with("200") {
            json!({"data": [{"embedding": [0.1, 0.2, 0.3, 0.4]}]}).to_string()
        } else {
            json!({"error": {"type": "unavailable", "message": "not up yet"}}).to_string()
        };
        let _ = socket.write_all(
            format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    }

    #[test]
    fn an_endpoint_on_this_machine_is_recognised_by_its_host() {
        for url in [
            "http://127.0.0.1:11434/v1/embeddings",
            "http://localhost:11434/v1/embeddings",
            "http://LOCALHOST/v1/embeddings",
            "http://[::1]:11434/v1/embeddings",
            "http://127.1.2.3/v1/embeddings",
        ] {
            assert!(is_loopback(url), "{url}");
        }
        for url in [
            "https://api.openai.com/v1/embeddings",
            "http://10.0.0.5:11434/v1/embeddings",
            "http://localhost.example.com/v1/embeddings",
            "not a url",
        ] {
            assert!(!is_loopback(url), "{url}");
        }
    }

    /// With Ollama stopped on Windows, a refused loopback connection took
    /// 2.06 s. Nothing listens on the port here; the refusal must come back
    /// well inside that, whichever platform runs the test.
    #[test]
    fn a_loopback_port_nothing_listens_on_is_given_up_on_quickly() {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("a free port")
            .port();
        let started = std::time::Instant::now();
        let refused = HostedEmbedder::connect(
            format!("http://127.0.0.1:{port}/v1/embeddings"),
            "nomic-embed-text",
            None,
        );
        assert!(refused.is_err());
        assert!(
            started.elapsed() < Duration::from_millis(1_500),
            "took {:?}",
            started.elapsed()
        );
    }

    /// Built right after a connection failed, the embedder does not ask again
    /// for the next query: that attempt is the one it was built after.
    #[test]
    fn an_embedder_built_after_a_failure_does_not_ask_again_at_once() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let address = listener.local_addr().expect("address");

        let embedder = Reconnecting::after_a_failed_attempt(
            format!("http://{address}/v1/embeddings"),
            "nomic-embed-text",
            None,
        );
        let said = embedder.embed("why sqlite").expect_err("not asked yet");
        assert!(said.contains("not asked again yet"), "{said}");
        assert!(
            listener.accept().is_err(),
            "nothing connected to the endpoint"
        );
    }

    /// The endpoint that was not up when the agent started, and is a moment
    /// later: the first query goes without vectors, a later one has them.
    #[test]
    fn an_endpoint_that_comes_up_later_is_used_once_it_answers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            // Down for the first probe, then a probe and an embedding.
            for status in ["503 Service Unavailable", "200 OK", "200 OK"] {
                let (mut socket, _) = listener.accept().expect("accept");
                answer(&mut socket, status);
            }
        });

        let embedder = Reconnecting::retrying_after(
            format!("http://{address}/v1/embeddings"),
            "nomic-embed-text",
            None,
            Duration::ZERO,
        );
        assert_eq!(
            embedder.model(),
            "nomic-embed-text",
            "named before it connects"
        );
        assert_eq!(embedder.dimension(), 0);

        let first = embedder.embed("why sqlite");
        assert!(first.is_err(), "the endpoint was down: {first:?}");

        let second = embedder
            .embed("why sqlite")
            .expect("connected on the next query");
        assert_eq!(second.len(), 4);
        assert_eq!(embedder.dimension(), 4);
        server.join().expect("server thread");
    }

    /// Down is not asked again on every query: each attempt can spend the
    /// whole timeout, and a query every few seconds would spend it every time.
    #[test]
    fn a_failed_connection_waits_before_it_is_tried_again() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            answer(&mut socket, "503 Service Unavailable");
        });

        let embedder = Reconnecting::retrying_after(
            format!("http://{address}/v1/embeddings"),
            "nomic-embed-text",
            None,
            Duration::from_secs(3600),
        );
        assert!(embedder.embed("one").is_err());
        server.join().expect("server thread");

        let waited = embedder.embed("two").expect_err("still waiting");
        assert!(waited.contains("not asked again yet"), "{waited}");
    }
}
