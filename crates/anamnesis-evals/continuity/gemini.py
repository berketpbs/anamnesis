"""One text request, pinned model, persisted reservation, no automatic retries."""
import argparse
import hashlib
import json
import os
import time
import urllib.error
import urllib.request
from pathlib import Path

from budget import Budget, count
from comparison import load, write_json

ENDPOINT = "https://generativelanguage.googleapis.com/v1beta/models/"


def request_json(url, key, body=None):
    data = None if body is None else json.dumps(body).encode("utf-8")
    request = urllib.request.Request(url, data=data, headers={
        "x-goog-api-key": key, "Content-Type": "application/json"})
    # Do not follow a redirect with a credential-bearing header.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    with urllib.request.build_opener(NoRedirect).open(request, timeout=60) as response:
        return json.load(response)


def generate(root: Path, prompt: str, output: Path, purpose: str, max_output: int,
             *, key: str, transport=request_json):
    manifest = load(root)
    if not key or not prompt or max_output <= 0:
        raise ValueError("credential, nonempty prompt and positive output limit required")
    if output.exists() or output.with_suffix(output.suffix + ".request.json").exists():
        raise ValueError("output/request path exists; never overwrite progress or retry implicitly")
    output.parent.mkdir(parents=True, exist_ok=True)
    model = manifest["pricing"]["model"]
    if not model.replace("-", "").replace(".", "").isalnum():
        raise ValueError("invalid pinned model ID")
    # Model metadata reads do not generate tokens. Reserve the full advertised
    # context/output ceilings, rather than assuming maxOutputTokens constrains
    # all thinking on every provider implementation.
    metadata = transport(ENDPOINT + model, key)
    if metadata.get("name") != "models/" + model:
        raise ValueError("model lookup returned a different model; no fallback")
    max_input, ceiling = count(metadata["inputTokenLimit"]), count(metadata["outputTokenLimit"])
    if not max_input or not ceiling or max_output > ceiling:
        raise ValueError("invalid advertised model limits or excessive output request")
    ledger = Budget(Path(manifest["budget_ledger"]), manifest["pricing"])
    try:
        reservation = ledger.reserve(model, max_input, ceiling, purpose)
        request_path = output.with_suffix(output.suffix + ".request.json")
        write_json(request_path, dict(reservation=reservation, model=model, purpose=purpose,
            prompt_sha256=hashlib.sha256(prompt.encode()).hexdigest(), model_metadata=metadata,
            requested_max_output=max_output, reserved_input=max_input, reserved_output=ceiling))
        started = time.monotonic()
        body = dict(contents=[dict(role="user", parts=[dict(text=prompt)])],
                    generationConfig=dict(maxOutputTokens=max_output, candidateCount=1),
                    serviceTier="standard")
        # A failed/uncertain request leaves its reservation in the shared ledger.
        response = transport(ENDPOINT + model + ":generateContent", key, body)
        response_path = output.with_suffix(output.suffix + ".response.json")
        write_json(response_path, response)
        if response.get("modelVersion") != model:
            raise ValueError("response version differs from the pinned model; retain reservation, verify identity")
        usage = response["usageMetadata"]
        input_tokens = count(usage["promptTokenCount"])
        total = count(usage["totalTokenCount"])
        output_tokens = total - input_tokens
        count(output_tokens)
        if output_tokens < count(usage.get("candidatesTokenCount", 0)) + count(usage.get("thoughtsTokenCount", 0)):
            raise ValueError("inconsistent usage metadata; retain reservation for reconciliation")
        if usage.get("toolUsePromptTokenCount", 0) or usage.get("serviceTier", "STANDARD").upper() != "STANDARD":
            raise ValueError("unpriced usage/service tier; retain reservation for reconciliation")
        ledger.settle(reservation, model, input_tokens, output_tokens)
        candidates = response.get("candidates", [])
        text = "".join(part.get("text", "") for candidate in candidates
                       for part in candidate.get("content", {}).get("parts", []) if not part.get("thought"))
        result = dict(reservation=reservation, model=model, model_version=response.get("modelVersion"),
                      input_tokens=input_tokens, output_tokens_including_thinking=output_tokens,
                      elapsed_seconds=time.monotonic() - started,
                      cost_usd=ledger.amount(input_tokens, output_tokens) / 1_000_000,
                      complete=bool(text) and all(c.get("finishReason") == "STOP" for c in candidates))
        write_json(output.with_suffix(output.suffix + ".usage.json"), result)
        with output.open("x", encoding="utf-8") as file:
            file.write(text)
        return result
    finally:
        ledger.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--prompt", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--purpose", choices=("access-check", "preparation", "summary", "brief", "retry"), required=True)
    parser.add_argument("--max-output", type=int, default=4096)
    args = parser.parse_args()
    try:
        result = generate(args.root, args.prompt.read_text(encoding="utf-8"), args.out,
                          args.purpose, args.max_output, key=os.environ.get("GEMINI_API_KEY", ""))
        print(json.dumps(result))
    except urllib.error.HTTPError as error:
        raise SystemExit(f"Gemini HTTP {error.code}; no retry; preserve any reservation and local progress") from None
    except (OSError, ValueError, KeyError) as error:
        # Never print URLs, request objects, credentials or provider error bodies.
        raise SystemExit(f"Gemini preparation stopped ({type(error).__name__}); inspect local ledger and traces") from None


if __name__ == "__main__":
    main()
