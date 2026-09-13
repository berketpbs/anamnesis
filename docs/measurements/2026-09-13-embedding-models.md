# Embedding models over the built-in suites, 2026-09-13

The table in `docs/DIRECTION.md` ("A longer window was measured"), and how to run
it again. Every run used a release build of `97be85d` and a data directory of its
own, so no model was downloaded into a live memory.

For each model, three comparisons over the four built-in suites:

```sh
export ANAMNESIS_DATA_DIR=/some/scratch/dir
export ANAMNESIS_EMBED_ENABLED=1
export ANAMNESIS_EMBED_MODEL=<model>

anamnesis eval --embed --compare vectors=0.25
anamnesis eval --embed --compare vectors=0
```

The `ships` row of either comparison is the model at the shipped tuning; the
`vectors=0` row is the same for every model, which is a check that the runs are
comparable.

Local models (fetched from Hugging Face into `$ANAMNESIS_DATA_DIR/models`):

- `sentence-transformers/all-MiniLM-L6-v2` — what ships
- `thenlper/gte-small`
- `thenlper/gte-base`
- `BAAI/bge-small-en-v1.5` — trained for CLS pooling; measured as this crate
  pools, which is by mean

nomic-embed-text, through Ollama (`ollama pull nomic-embed-text`):

```sh
export ANAMNESIS_EMBED_PROVIDER=openai
export ANAMNESIS_EMBED_URL=http://127.0.0.1:11434/v1/embeddings
export ANAMNESIS_EMBED_API_KEY=ollama   # Ollama ignores it
export ANAMNESIS_EMBED_MODEL=nomic-embed-text
```

No `search_query:` / `search_document:` prefix was added, which is how the model
card asks to be used; nothing in the hosted embedder adds one. The hosted path
reports no truncation, so `eval --embed` says every page was read whole — true
here, where the longest page is 537 tokens, and not a claim it can check.

With nomic-embed-text, `eval --embed --verbose` names what is still missed on
`long`: `units reboot every morning at first light`, `what has to happen before
the boot report goes out`, `when is new firmware trusted to keep running`, `what
do we do with a unit whose secret never reached the network server` and `why is
the adjustment tied to the part instead of where it is installed` return nothing
relevant in the first five.
