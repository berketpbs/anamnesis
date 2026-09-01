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
//! [`crate::openai`] exists rather than a provider per company.
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
        let client = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|error| EmbedError::Fetch {
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
}
