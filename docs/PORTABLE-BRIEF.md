# A portable continuation file

```text
anamnesis brief --out durum.md
anamnesis brief --workstream auth-refactor --out auth-durum.md
```

The file contains the last captured user/assistant messages in the selected
slot, its retained handoff, current decision-page excerpts (including recorded
rejections), timestamps, page revisions and source references. A missing
message or next step is reported as missing rather than invented. Stored notes
are framed as historical evidence to check. Newer captured work is identified
when the retained handoff describes an older session.

Generation is deterministic and makes no model request. Current redaction is
applied before clipping excerpts and again to the final file. Superseded,
expired and non-active decisions are excluded; session-bound decisions from
another workstream or operator are excluded. No handoff is claimed, accepted
or discarded, and wiki access counters/history are not changed. A file is
written locally; Git staging, publishing and external delivery are explicit
actions outside this command.

A per-user project requires `--operator NAME`; supplying an operator to a
shared-slot project is rejected. Existing output files are preserved unless
`--force` is passed. The command writes through a temporary file in the output
directory and publishes it atomically. It does not create output directories.

Excerpts are bounded (4 KB per captured message, 6 KB for the handoff, 3 KB
per decision and 20 selected decision pages after inspecting up to 200 index
candidates). Excerpt/capture truncation is explicit. Source page links point
to local wiki files: they remain provenance when the brief moves to another
machine, but the other machine needs those files to open them. No raw archive
or original source file is automatically copied into the brief.
