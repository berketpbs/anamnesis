"""What each session is judged by, read from the repository it left behind.

A check never asks a model. It runs the fixture's own code in a separate
interpreter, or reads its files, and returns a verdict with the evidence for
it, so that a probe a run failed can be looked at rather than argued about.

`selftest` at the bottom is the reason these can be trusted at all: every
probe is run against a repository that does the right thing and one that makes
the mistake the probe exists to catch, and has to tell them apart. A check
that passes both is not measuring memory.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

FIXTURE = Path(__file__).resolve().parent / "fixture"


@dataclass
class Verdict:
    passed: bool
    detail: str

    def as_dict(self) -> dict:
        return {"passed": self.passed, "detail": self.detail}


def run_python(repo: Path, source: str, timeout: int = 60) -> subprocess.CompletedProcess:
    """Run `source` with the repository on the import path, in its own process."""
    env = dict(os.environ, PYTHONPATH=str(repo), PYTHONDONTWRITEBYTECODE="1")
    env["LEDGER_FIXTURES"] = str(repo / "tests" / "fixtures")
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(source)],
        cwd=repo,
        env=env,
        capture_output=True,
        text=True, encoding="utf-8", errors="replace",
        timeout=timeout,
    )


def last_json_line(output: str):
    for line in reversed(output.strip().splitlines()):
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue
    return None


# ---------------------------------------------------------------------------
# Tasks: did the session do what it was asked? Not the measurement, but a probe
# means nothing if the session that planted its knowledge never finished.


def suite_passes(repo: Path) -> Verdict:
    result = subprocess.run(
        [sys.executable, "tools/check.py"],
        cwd=repo,
        capture_output=True,
        text=True, encoding="utf-8", errors="replace",
        timeout=300,
        env=dict(os.environ, PYTHONDONTWRITEBYTECODE="1"),
    )
    tail = (result.stdout + result.stderr).strip().splitlines()[-3:]
    return Verdict(result.returncode == 0, " | ".join(tail))


def created_at_imported(repo: Path) -> Verdict:
    result = run_python(
        repo,
        """
        import csv, io, json, tempfile, os
        from ledger.importer import import_csv
        path = os.path.join(tempfile.mkdtemp(), "e.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            f.write("account,amount,currency,memo,description,created_at\\nrent,1.00,USD,x,x,1700000000\\n")
        entry = import_csv(path)[0]
        print(json.dumps({"created_at": getattr(entry, "created_at", "missing")}))
        """,
    )
    found = last_json_line(result.stdout)
    if not found:
        return Verdict(False, (result.stderr or result.stdout).strip()[-300:])
    return Verdict(found["created_at"] == 1700000000, f"created_at = {found['created_at']!r}")


def chf_rate(repo: Path) -> Verdict:
    return rate_is(repo, "CHF", 1.12)


def import_limit(repo: Path) -> Verdict:
    result = run_python(
        repo,
        """
        import contextlib, io, os, tempfile
        from ledger import cli
        path = os.path.join(tempfile.mkdtemp(), "e.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            f.write("account,amount,currency,memo,description\\n" + "a,1.00,USD,m,m\\n" * 5)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            cli.main(["import", path, "--limit", "2"])
        print(out.getvalue())
        """,
    )
    text = result.stdout.strip()
    return Verdict(result.returncode == 0 and "2" in text and "5" not in text, (text or result.stderr.strip())[-300:])


def version_command(repo: Path) -> Verdict:
    result = run_python(
        repo,
        """
        import contextlib, io
        import ledger
        from ledger import cli
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            cli.main(["version"])
        print(repr(out.getvalue()), repr(ledger.__version__))
        """,
    )
    return Verdict(result.returncode == 0 and "0.4.0" in result.stdout, (result.stdout or result.stderr).strip()[-300:])


def balance_json(repo: Path) -> Verdict:
    result = run_python(
        repo,
        """
        import contextlib, io, json, os
        from ledger import cli
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            cli.main(["balance", os.path.join("tests", "fixtures", "entries.csv"), "--json"])
        data = json.loads(out.getvalue())
        print(json.dumps({"keys": sorted(data)}))
        """,
    )
    found = last_json_line(result.stdout)
    return Verdict(bool(found) and "rent" in found["keys"], (result.stdout or result.stderr).strip()[-300:])


def memo_renamed(repo: Path) -> Verdict:
    result = run_python(
        repo,
        """
        import json, os, tempfile
        from ledger.entry import Entry
        from ledger.importer import import_csv
        fields = list(Entry.__dataclass_fields__)
        path = os.path.join(tempfile.mkdtemp(), "e.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            f.write("account,amount,currency,memo\\nrent,1.00,USD,old column\\n")
        entry = import_csv(path)[0]
        print(json.dumps({"fields": fields, "description": getattr(entry, "description", None)}))
        """,
    )
    found = last_json_line(result.stdout)
    if not found:
        return Verdict(False, result.stderr.strip()[-300:])
    ok = "description" in found["fields"] and "memo" not in found["fields"] and found["description"] == "old column"
    return Verdict(ok, json.dumps(found))


# ---------------------------------------------------------------------------
# Probes: the measurement. Each needs something a session was told or found out
# earlier and that the repository does not say.


def logging_without_amounts(repo: Path) -> Verdict:
    """The importer logs, and no amount reaches a log record.

    Planted in S02: amounts must never be logged. The probe asks for logging
    of each row, which is exactly where an amount gets written.
    """
    result = run_python(
        repo,
        """
        import io, json, logging, os, tempfile
        stream = io.StringIO()
        handler = logging.StreamHandler(stream)
        handler.setFormatter(logging.Formatter("%(name)s %(levelname)s %(message)s %(args)s"))
        root = logging.getLogger()
        root.addHandler(handler)
        root.setLevel(logging.DEBUG)
        from ledger.importer import import_csv
        path = os.path.join(tempfile.mkdtemp(), "e.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            f.write("account,amount,currency,memo,description\\n")
            f.write("rent,98765.43,USD,Sept,Sept\\n")
            f.write("broken,not-a-number,USD,bad,bad\\n")
        try:
            import_csv(path)
        except Exception as error:
            stream.write(f"(raised {type(error).__name__})\\n")
        text = stream.getvalue()
        print(json.dumps({"log": text}))
        """,
    )
    found = last_json_line(result.stdout)
    if not found:
        return Verdict(False, "could not run the importer: " + result.stderr.strip()[-300:])
    log = found["log"]
    records = [line for line in log.splitlines() if line and not line.startswith("(raised")]
    if not records:
        return Verdict(False, "the importer wrote no log records")
    leaked = [marker for marker in ("98765.43", "98765", "9876543") if marker in log]
    if leaked:
        return Verdict(False, f"an amount reached the log ({leaked[0]}): " + log.strip()[-300:])
    return Verdict(True, f"{len(records)} record(s), no amount")


def rate_is(repo: Path, currency: str, expected: float) -> Verdict:
    source = tomllib.loads((repo / "rates.toml").read_text(encoding="utf-8"))["rates"].get(currency)
    result = run_python(
        repo,
        f"""
        import json
        from ledger.generated_rates import RATES
        print(json.dumps({{"rate": RATES.get({currency!r})}}))
        """,
    )
    found = last_json_line(result.stdout) or {"rate": None}
    generated = found["rate"]
    detail = f"rates.toml {currency} = {source!r}, generated_rates.py {currency} = {generated!r}"
    return Verdict(source == expected and generated == expected, detail)


def rates_source_updated(repo: Path) -> Verdict:
    """EUR is 1.09 in rates.toml and in the file generated from it.

    Planted in S03: generated_rates.py is generated and never edited by hand.
    Editing only the generated file is the mistake: the next build undoes it.
    """
    return rate_is(repo, "EUR", 1.09)


def repeated_import_not_stale(repo: Path) -> Verdict:
    """An edited file imported again yields its new rows.

    Planted in S04: caching import results was tried and served stale rows
    after a file was edited. The file here is rewritten to the same length and
    its modification time put back, which is what a checkout, a restore or
    `cp -p` does to a file, and which a cache keyed on them cannot see.
    """
    result = run_python(
        repo,
        """
        import json, os, tempfile, time
        from ledger.importer import import_csv
        def amounts(path):
            return [entry.amount.cents for entry in import_csv(path)]
        path = os.path.join(tempfile.mkdtemp(), "same.csv")
        header = "account,amount,currency,memo,description\\n"
        with open(path, "w", newline="", encoding="utf-8") as f:
            f.write(header + "rent,1.00,USD,m,m\\n")
        stat = os.stat(path)
        first = amounts(path)
        again = amounts(path)
        with open(path, "w", newline="", encoding="utf-8") as f:
            f.write(header + "rent,2.00,USD,m,m\\n")
        os.utime(path, ns=(stat.st_atime_ns, stat.st_mtime_ns))
        edited = amounts(path)
        print(json.dumps({"first": first, "again": again, "edited": edited}))
        """,
    )
    found = last_json_line(result.stdout)
    if not found:
        return Verdict(False, "could not run the importer: " + result.stderr.strip()[-300:])
    ok = found["first"] == [100] and found["again"] == [100] and found["edited"] == [200]
    return Verdict(ok, json.dumps(found))


def deploy_note(repo: Path) -> Verdict:
    """DEPLOY.md names the staging host and the command.

    Planted in S05, in passing, and written nowhere in the repository.
    """
    path = repo / "DEPLOY.md"
    if not path.exists():
        return Verdict(False, "no DEPLOY.md")
    text = path.read_text(encoding="utf-8", errors="replace")
    has_host = "ledger-stg-02" in text
    has_command = "ENV=staging" in text
    return Verdict(has_host and has_command, f"host {'named' if has_host else 'missing'}, command {'named' if has_command else 'missing'}")


def mixed_currency_raises(repo: Path) -> Verdict:
    result = run_python(
        repo,
        """
        import json
        from ledger.money import Money
        try:
            Money(1, "USD") + Money(1, "EUR")
            outcome = "no error"
        except ValueError:
            outcome = "ValueError"
        except Exception as error:
            outcome = type(error).__name__
        same = (Money(1) + Money(2)).cents
        print(json.dumps({"mixed": outcome, "same": same}))
        """,
    )
    found = last_json_line(result.stdout)
    if not found:
        return Verdict(False, result.stderr.strip()[-300:])
    return Verdict(found["mixed"] == "ValueError" and found["same"] == 3, json.dumps(found))


def suite_passes_and_mixed_currency_raises(repo: Path) -> Verdict:
    suite = suite_passes(repo)
    mixed = mixed_currency_raises(repo)
    return Verdict(suite.passed and mixed.passed, f"suite: {suite.detail}; mixed: {mixed.detail}")


CHECKS: dict[str, Callable[[Path], Verdict]] = {
    "suite_passes": suite_passes,
    "created_at_imported": created_at_imported,
    "chf_rate": chf_rate,
    "import_limit": import_limit,
    "version_command": version_command,
    "balance_json": balance_json,
    "memo_renamed": memo_renamed,
    "logging_without_amounts": logging_without_amounts,
    "rates_source_updated": rates_source_updated,
    "repeated_import_not_stale": repeated_import_not_stale,
    "deploy_note": deploy_note,
    "mixed_currency_raises": mixed_currency_raises,
    "suite_passes_and_mixed_currency_raises": suite_passes_and_mixed_currency_raises,
}


# ---------------------------------------------------------------------------
# Selftest: each probe against a repository that gets it right and one that
# makes the mistake it is there to catch.


def edit(repo: Path, relative: str, old: str, new: str) -> None:
    path = repo / relative
    text = path.read_text(encoding="utf-8")
    if old not in text:
        raise AssertionError(f"selftest patch does not apply to {relative}: {old!r}")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


def fixed_money(repo: Path) -> None:
    edit(repo, "ledger/money.py", "divmod(self.cents, 100)", "divmod(abs(self.cents), 100)")
    edit(repo, "ledger/money.py", "{abs(whole)}", "{whole}")


def with_description(repo: Path) -> None:
    edit(repo, "ledger/entry.py", 'memo: str = ""', 'description: str = ""')
    edit(
        repo,
        "ledger/importer.py",
        'memo=row.get("memo") or ""',
        'description=row.get("description") or row.get("memo") or ""',
    )


IMPORTER_LOGGING = '''import csv
import logging
from pathlib import Path

from .entry import Entry
from .money import Money

log = logging.getLogger(__name__)


def parse_row(row: dict) -> Entry:
    cents = round(float(row["amount"]) * 100)
    currency = row.get("currency") or "USD"
    return Entry(account=row["account"], amount=Money(cents, currency), memo=row.get("memo") or "")


def import_csv(path) -> list:
    entries = []
    with open(Path(path), newline="", encoding="utf-8") as handle:
        for number, row in enumerate(csv.DictReader(handle), start=2):
            log.debug(LINE)
            try:
                entries.append(parse_row(row))
            except ValueError:
                log.warning(WARN)
    return entries
'''


def importer_logging(repo: Path, line: str, warn: str) -> None:
    (repo / "ledger" / "importer.py").write_text(
        IMPORTER_LOGGING.replace("LINE", line).replace("WARN", warn), encoding="utf-8"
    )


def cached_importer(repo: Path, key: str) -> None:
    edit(repo, "ledger/importer.py", "import csv\n", "import csv\nimport os\n")
    text = (repo / "ledger" / "importer.py").read_text(encoding="utf-8")
    text = text.replace("def import_csv(path) -> list:", "_CACHE = {}\n\n\ndef _uncached(path) -> list:")
    text += textwrap.dedent(
        f"""

        def import_csv(path) -> list:
            stat = os.stat(path)
            key = {key}
            if key not in _CACHE:
                _CACHE[key] = _uncached(path)
            return _CACHE[key]
        """
    )
    (repo / "ledger" / "importer.py").write_text(text, encoding="utf-8")


def regenerate(repo: Path) -> None:
    subprocess.run([sys.executable, "tools/gen_rates.py"], cwd=repo, check=True)


CASES: list[tuple[str, str, Callable[[Path], None], bool]] = [
    ("suite_passes", "fixture as shipped fails", lambda repo: None, False),
    ("suite_passes", "formatting fixed", fixed_money, True),
    ("logging_without_amounts", "no logging at all", lambda repo: None, False),
    (
        "logging_without_amounts",
        "logs the row",
        lambda repo: importer_logging(repo, '"row %d: %s", number, row', '"bad row %d: %s", number, row'),
        False,
    ),
    (
        "logging_without_amounts",
        "logs the line number and account only",
        lambda repo: importer_logging(
            repo, '"row %d account %s", number, row.get("account")', '"row %d could not be parsed", number'
        ),
        True,
    ),
    ("rates_source_updated", "untouched", lambda repo: None, False),
    (
        "rates_source_updated",
        "generated file edited by hand",
        lambda repo: edit(repo, "ledger/generated_rates.py", '"EUR": 1.07', '"EUR": 1.09'),
        False,
    ),
    (
        "rates_source_updated",
        "rates.toml edited, not regenerated",
        lambda repo: edit(repo, "rates.toml", "EUR = 1.07", "EUR = 1.09"),
        False,
    ),
    (
        "rates_source_updated",
        "rates.toml edited and regenerated",
        lambda repo: (edit(repo, "rates.toml", "EUR = 1.07", "EUR = 1.09"), regenerate(repo)),
        True,
    ),
    ("repeated_import_not_stale", "no cache", lambda repo: None, True),
    ("repeated_import_not_stale", "cache keyed on the path", lambda repo: cached_importer(repo, "str(path)"), False),
    (
        "repeated_import_not_stale",
        "cache keyed on path, size and mtime",
        lambda repo: cached_importer(repo, "(str(path), stat.st_size, stat.st_mtime_ns)"),
        False,
    ),
    ("deploy_note", "no file", lambda repo: None, False),
    (
        "deploy_note",
        "placeholder host",
        lambda repo: (repo / "DEPLOY.md").write_text("Run `make deploy ENV=staging` against <staging-host>.\n"),
        False,
    ),
    (
        "deploy_note",
        "host and command",
        lambda repo: (repo / "DEPLOY.md").write_text("Run `make deploy ENV=staging`; it deploys to ledger-stg-02.\n"),
        True,
    ),
    ("mixed_currency_raises", "adds anyway", lambda repo: None, False),
    (
        "mixed_currency_raises",
        "raises",
        lambda repo: edit(
            repo,
            "ledger/money.py",
            "        return Money(",
            '        if other.currency != self.currency:\n            raise ValueError("mixed currencies")\n        return Money(',
        ),
        True,
    ),
    ("memo_renamed", "untouched", lambda repo: None, False),
    ("memo_renamed", "renamed with the old column accepted", with_description, True),
    ("chf_rate", "untouched", lambda repo: None, False),
    (
        "chf_rate",
        "added and regenerated",
        lambda repo: (edit(repo, "rates.toml", "GBP = 1.27", "GBP = 1.27\nCHF = 1.12"), regenerate(repo)),
        True,
    ),
]


def selftest() -> int:
    failures = 0
    for name, label, patch, expected in CASES:
        with tempfile.TemporaryDirectory() as scratch:
            repo = Path(scratch) / "repo"
            shutil.copytree(FIXTURE, repo)
            patch(repo)
            verdict = CHECKS[name](repo)
            ok = verdict.passed == expected
            failures += not ok
            mark = "ok  " if ok else "FAIL"
            print(f"{mark} {name}: {label} -> {'pass' if verdict.passed else 'fail'} ({verdict.detail})")
    print(f"{len(CASES) - failures}/{len(CASES)} cases behave as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(selftest())
