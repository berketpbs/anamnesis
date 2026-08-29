//! Local text embeddings, for the vector-cosine retrieval stream.
//!
//! Unlike [`crate::Provider`], which asks a remote API for a completion, an
//! [`Embedder`] runs entirely on this machine: no network round trip per
//! query, no API key, no per-call cost. What it costs instead is a one-time
//! model download and the CPU time to run inference — small for a
//! sentence-embedding model, but real, which is why (like `Provider`) it is
//! never mandatory. Nothing in this crate requires one, `memory_query`'s
//! vector stream simply does not run without one, and the caller decides
//! whether to build one at all.
//!
//! The one implementation here, [`LocalEmbedder`], loads a BERT-family
//! sentence-embedding model via [`candle`](https://github.com/huggingface/candle)
//! and mean-pools its token outputs into one L2-normalized vector — the same
//! recipe `sentence-transformers` models are trained to be used with.

use std::path::Path;
use std::sync::Arc;

use anamnesis_core::embedding::Embed;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use tokenizers::Tokenizer;

/// Hugging Face repo id used when no model is configured explicitly.
pub const DEFAULT_MODEL: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Errors specific to building or running a local embedder.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// The model's files could not be fetched (no cached copy, and either no
    /// network or the repo does not exist).
    #[error("could not fetch model {model:?}: {reason}")]
    Fetch {
        /// The repo id that was asked for.
        model: String,
        /// What went wrong.
        reason: String,
    },
    /// The files fetched did not describe a usable model.
    #[error("could not load model {model:?}: {reason}")]
    Load {
        /// The repo id that was asked for.
        model: String,
        /// What went wrong.
        reason: String,
    },
    /// The input text could not be tokenized.
    #[error("could not tokenize text: {0}")]
    Tokenize(String),
    /// Running the model failed.
    #[error("embedding inference failed: {0}")]
    Inference(String),
}

/// Something that turns text into a fixed-size, L2-normalized vector.
///
/// Built on [`Embed`], which is what the index writes with: naming the model
/// and producing a vector are the whole of what storing one requires, and
/// keeping that pair in `anamnesis-core` is what spares the storage layer a
/// dependency on a machine-learning toolchain. An embedder is that, plus the
/// dimension nothing but this crate needs.
pub trait Embedder: Embed {
    /// Length of the vector this embedder produces.
    fn dimension(&self) -> usize;
}

/// A BERT-family sentence-embedding model, running locally on CPU via candle.
pub struct LocalEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    model_id: String,
    dim: usize,
    /// The model's positional embedding table has exactly this many rows;
    /// tokenizing more text than this and handing it to `forward` would index
    /// past the end of that table rather than degrade gracefully.
    max_tokens: usize,
}

impl LocalEmbedder {
    /// Load a model, fetching its files into `cache_dir` on first use.
    ///
    /// Subsequent calls with the same `cache_dir` reuse what was downloaded;
    /// this is what makes `<data_dir>/models/` a cache rather than a
    /// one-shot download directory.
    pub fn load(model_id: &str, cache_dir: &Path) -> Result<Self, EmbedError> {
        let device = Device::Cpu;

        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("anamnesis/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| EmbedError::Fetch {
                model: model_id.to_owned(),
                reason: error.to_string(),
            })?;

        let config_path = fetch_cached(&client, model_id, "config.json", cache_dir)?;
        let tokenizer_path = fetch_cached(&client, model_id, "tokenizer.json", cache_dir)?;
        let weights_path = fetch_cached(&client, model_id, "model.safetensors", cache_dir)?;

        let load = |reason: String| EmbedError::Load {
            model: model_id.to_owned(),
            reason,
        };

        let config_text = std::fs::read_to_string(&config_path).map_err(|e| load(e.to_string()))?;
        let config: Config = serde_json::from_str(&config_text).map_err(|e| load(e.to_string()))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| load(e.to_string()))?;

        let weights = std::fs::read(&weights_path).map_err(|e| load(e.to_string()))?;
        let vb = VarBuilder::from_buffered_safetensors(weights, DTYPE, &device)
            .map_err(|e| load(e.to_string()))?;
        let model = BertModel::load(vb, &config).map_err(|e| load(e.to_string()))?;

        Ok(Self {
            model,
            tokenizer,
            device,
            model_id: model_id.to_owned(),
            dim: config.hidden_size,
            max_tokens: config.max_position_embeddings,
        })
    }
}

/// Fetch one file from a Hugging Face model repo's `main` branch, caching it
/// under `cache_dir` so a second `load` of the same model touches no network.
///
/// This is a small hand-rolled client rather than the `hf-hub` crate's own
/// sync API: that API's `ureq` backend does not follow the *relative*
/// redirect the Hugging Face CDN answers file requests with, and fails every
/// download outright. `reqwest` follows it correctly, and this crate already
/// depends on it for the Anthropic provider.
fn fetch_cached(
    client: &reqwest::blocking::Client,
    model_id: &str,
    file: &str,
    cache_dir: &Path,
) -> Result<std::path::PathBuf, EmbedError> {
    let dest_dir = cache_dir.join(model_id.replace('/', "--"));
    std::fs::create_dir_all(&dest_dir).map_err(|error| EmbedError::Fetch {
        model: model_id.to_owned(),
        reason: format!("{file}: {error}"),
    })?;
    let dest = dest_dir.join(file);
    if dest.is_file() {
        return Ok(dest);
    }

    let fetch_err = |reason: String| EmbedError::Fetch {
        model: model_id.to_owned(),
        reason: format!("{file}: {reason}"),
    };

    let url = format!("https://huggingface.co/{model_id}/resolve/main/{file}");
    let response = client
        .get(&url)
        .send()
        .map_err(|e| fetch_err(e.to_string()))?;
    if !response.status().is_success() {
        return Err(fetch_err(format!("http {}", response.status())));
    }
    let bytes = response.bytes().map_err(|e| fetch_err(e.to_string()))?;

    // Downloaded under a temporary name and renamed into place, so a process
    // killed mid-download leaves no file at `dest` for a later `load` to find
    // and mistake for a complete one.
    let temp = dest_dir.join(format!(".{file}.tmp"));
    std::fs::write(&temp, &bytes).map_err(|e| fetch_err(e.to_string()))?;
    std::fs::rename(&temp, &dest).map_err(|e| fetch_err(e.to_string()))?;
    Ok(dest)
}

impl Embedder for LocalEmbedder {
    fn dimension(&self) -> usize {
        self.dim
    }
}

impl Embed for LocalEmbedder {
    /// Recorded alongside every vector this writes, so a later query never
    /// compares vectors from two different embedding spaces.
    fn model(&self) -> &str {
        &self.model_id
    }

    /// Tokenize, run the encoder, mean-pool the token outputs, and
    /// L2-normalize — the standard recipe `sentence-transformers` models are
    /// trained against, and the only one this crate implements.
    ///
    /// The typed failures are kept internally and flattened here: nothing
    /// above this distinguishes a tokenizer fault from an inference one, and
    /// both cost a page the same thing.
    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|error| EmbedError::Tokenize(error.to_string()).to_string())?;

        let inference = || -> candle_core::Result<Tensor> {
            let ids = &encoding.get_ids()[..encoding.get_ids().len().min(self.max_tokens)];
            let input_ids = Tensor::new(ids, &self.device)?.unsqueeze(0)?;
            let token_type_ids = input_ids.zeros_like()?;

            // A single, unpadded sequence: leaving the attention mask as
            // `None` makes the model default to all-ones, which is exactly
            // right here since there is no padding to mask out.
            let sequence_output = self.model.forward(&input_ids, &token_type_ids, None)?;

            let seq_len = sequence_output.dims()[1] as f64;
            let pooled = sequence_output
                .sum(1)?
                .affine(1.0 / seq_len, 0.0)?
                .squeeze(0)?;
            let norm = pooled.sqr()?.sum_all()?.sqrt()?;
            pooled.broadcast_div(&norm)
        };

        inference()
            .and_then(|tensor| tensor.to_vec1::<f32>())
            .map_err(|error| EmbedError::Inference(error.to_string()).to_string())
    }
}

/// Environment-driven configuration for building an [`Embedder`].
///
/// Disabled by default. Unlike the LLM provider, there is no key whose mere
/// presence implies intent — so turning this on is an explicit opt-in, the
/// same way a first-time download of ~90MB of model weights ought to be.
#[derive(Debug, Clone)]
pub struct EmbedConfig {
    /// Whether the vector-cosine stream should run at all.
    pub enabled: bool,
    /// Hugging Face repo id to load when enabled.
    pub model: String,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: DEFAULT_MODEL.to_owned(),
        }
    }
}

impl EmbedConfig {
    /// Read settings from the process environment.
    pub fn from_env() -> Self {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// Enabled, taking the model from the environment if it names one.
    ///
    /// For a caller that has already been asked for the embedder in its own
    /// words — `anamnesis eval --embed` — where making them also set the
    /// environment variable would be asking twice.
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::from_env()
        }
    }

    /// Read settings from an arbitrary lookup, so this is testable without
    /// mutating the environment of a parallel test run.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let enabled = var("ANAMNESIS_EMBED_ENABLED")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            })
            .unwrap_or(false);
        let model = var("ANAMNESIS_EMBED_MODEL")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        Self { enabled, model }
    }

    /// Build a local embedder, when embeddings are enabled.
    ///
    /// `None` is a successful outcome, same as [`crate::LlmConfig::build`]:
    /// "no embedder" is what "run without the vector stream" looks like, not
    /// an error. Loading can still fail once enabled — the model could not be
    /// fetched, or the cached files are corrupt — and that *is* an error,
    /// since the caller asked for it explicitly.
    pub fn build(&self, cache_dir: &Path) -> Result<Option<Arc<dyn Embedder>>, EmbedError> {
        if !self.enabled {
            return Ok(None);
        }
        Ok(Some(Arc::new(LocalEmbedder::load(&self.model, cache_dir)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| {
            owned
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.to_owned())
        }
    }

    #[test]
    fn an_empty_environment_is_disabled() {
        let config = EmbedConfig::from_vars(vars(&[]));
        assert!(!config.enabled);
        assert_eq!(config.model, DEFAULT_MODEL);
    }

    #[test]
    fn an_explicit_enable_turns_it_on() {
        let config = EmbedConfig::from_vars(vars(&[("ANAMNESIS_EMBED_ENABLED", "1")]));
        assert!(config.enabled);
    }

    #[test]
    fn a_model_override_is_honoured_only_when_enabled_is_irrelevant_to_it() {
        // The model name and the enable flag are independent: naming a model
        // does not itself turn the stream on.
        let config = EmbedConfig::from_vars(vars(&[("ANAMNESIS_EMBED_MODEL", "org/other-model")]));
        assert!(!config.enabled);
        assert_eq!(config.model, "org/other-model");
    }

    #[test]
    fn a_blank_model_override_falls_back_to_the_default() {
        let config = EmbedConfig::from_vars(vars(&[
            ("ANAMNESIS_EMBED_ENABLED", "true"),
            ("ANAMNESIS_EMBED_MODEL", "   "),
        ]));
        assert_eq!(config.model, DEFAULT_MODEL);
    }

    #[test]
    fn disabled_builds_to_no_embedder_without_touching_the_network() {
        let config = EmbedConfig::from_vars(vars(&[]));
        let embedder = config.build(Path::new("/nonexistent")).expect("no error");
        assert!(embedder.is_none());
    }

    /// Downloads the real default model and runs real inference. Not part of
    /// the normal suite — it needs network and ~90MB on first run — but it is
    /// the only test that would catch a wrong tensor shape or a pooling bug
    /// the way a fake `Embedder` never could. Run with
    /// `cargo test -p anamnesis-llm -- --ignored embed::tests::the_default_model`.
    #[test]
    #[ignore = "downloads a model from the network on first run"]
    fn the_default_model_produces_sane_normalized_vectors() {
        let cache = tempfile::tempdir().expect("tempdir");
        let embedder = LocalEmbedder::load(DEFAULT_MODEL, cache.path()).expect("load");
        assert_eq!(embedder.dimension(), 384);

        let cat = embedder.embed("a cat sitting on a mat").expect("embed");
        let dog = embedder.embed("a dog resting on a rug").expect("embed");
        let invoice = embedder
            .embed("quarterly tax filing deadline")
            .expect("embed");

        assert_eq!(cat.len(), 384);
        let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm(&cat) - 1.0).abs() < 1e-3,
            "vectors should be L2-normalized"
        );

        let cosine = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let related = cosine(&cat, &dog);
        let unrelated = cosine(&cat, &invoice);
        assert!(
            related > unrelated,
            "semantically related sentences ({related}) should score above unrelated ones ({unrelated})"
        );

        // A page body far longer than the model's 512-token window must be
        // truncated internally rather than panicking or erroring.
        let huge = "the quick brown fox jumps over the lazy dog. ".repeat(200);
        let vector = embedder.embed(&huge).expect("embed a too-long input");
        assert_eq!(vector.len(), 384);
    }
}
