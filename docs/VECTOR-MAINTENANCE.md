# Inspecting old embedding models

The index can retain vectors and failure records under several models after a
model switch. `doctor` names these remnants when the running server identifies
its current embedder. It does not guess which stored model is active when the
server cannot answer.

```text
anamnesis vectors prune
anamnesis vectors prune --model OLD_MODEL --apply
anamnesis vectors prune --server http://127.0.0.1:8080 --apply
```

The default is a preview with row counts and stored vector blob bytes. Models
selected by local configuration and by the matching server are protected;
an explicit `--model` cannot override that protection. A local model's full
repository id and short runtime id are both protected. No model is downloaded
or queried while preparing the preview.

Apply requires the matching server's `/whoami` to explicitly report its
embedding selection (a model name or null when disabled). Missing, refused or
old responses leave the preview readable but refuse removal. The selection
is queried again immediately before removal; a change refuses mutation.

Removal affects only this project's page/section vectors, abstract vectors
and failure rows under selected inactive models. Other projects, active model
rows, wiki/raw sources and downloaded model files remain outside the operation.
Each model is removed in a database transaction; interrupted multi-model
cleanup can be previewed and repeated. Recreating vectors may require model
downloads or hosted API calls. Pruning does not VACUUM the database, so reported
blob bytes are not a promised reduction in file size.
