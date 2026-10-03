-- A tool call's input, indexed once.
--
-- A harness that reports a call before and after it runs sends the input
-- twice: the attempt carries it, and the completion carries it again with the
-- tail of the result behind it. Nothing reads the attempt's copy once the
-- completion is in, and on a real install those copies were a third of all
-- observation text. Capture now stores such an attempt with an empty body as
-- the second half arrives, and `reindex` applies the same rule after
-- replaying a session; this applies it to what was recorded before.
--
-- The rule, as `Store::settle_tool_calls` states it: an attempt with a call
-- id gives up its body when its session holds the completion of the same call
-- and that completion begins with exactly the attempt's body. Its row stays,
-- and an attempt whose call never came back keeps what it said. The
-- transcripts under raw/ are not touched; they keep both bodies whole.
UPDATE observations SET body = '', truncated = 0
WHERE kind = 'tool-attempt'
  AND tool_call_id IS NOT NULL
  AND body <> ''
  AND EXISTS (
      SELECT 1 FROM observations AS done
      WHERE done.session_id = observations.session_id
        AND done.tool_call_id = observations.tool_call_id
        AND done.kind = 'tool-use'
        AND substr(done.body, 1, length(observations.body)) = observations.body
  );
