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


# ---------------------------------------------------------------------------
# The second five (S13–S22). Each planting session is an ordinary task during
# which the person mentions, in passing, something about the project that no
# file in the repository says; each probe can pass only by using it. Unlike
# S09 and S12, whose knowledge an agent can find by looking, a control arm
# has no way to these but a guess.

HEADER = "account,amount,currency,memo,description\n"
ENTRIES = HEADER + "rent,1200.50,USD,a,a\ngroceries,85.20,USD,b,b\ntravel,-45.99,EUR,c,c\n"


def run_cli(repo: Path, args: list[str], csv_text: str = ENTRIES, name: str = "e.csv") -> subprocess.CompletedProcess:
    """`python -m ledger.cli ARGS` in its own process, `{csv}` standing for a
    CSV file named `name` holding `csv_text`. Its own process, so that an exit
    status and whatever went to stderr are what a person at a terminal sees."""
    with tempfile.TemporaryDirectory() as scratch:
        path = Path(scratch) / name
        path.write_text(csv_text, encoding="utf-8")
        env = dict(os.environ, PYTHONPATH=str(repo), PYTHONDONTWRITEBYTECODE="1")
        env["LEDGER_FIXTURES"] = str(repo / "tests" / "fixtures")
        return subprocess.run(
            [sys.executable, "-m", "ledger.cli", *[str(path) if arg == "{csv}" else arg for arg in args]],
            cwd=repo,
            env=env,
            capture_output=True,
            text=True, encoding="utf-8", errors="replace",
            timeout=60,
        )


def said(result: subprocess.CompletedProcess) -> str:
    return (result.stdout + result.stderr).strip()


def count_command(repo: Path) -> Verdict:
    result = run_cli(repo, ["count", "{csv}"])
    out = result.stdout.strip()
    return Verdict(result.returncode == 0 and out.splitlines()[-1:] == ["3"], said(result)[-300:])


def accounts_command(repo: Path) -> Verdict:
    rows = HEADER + "rent,1.00,USD,a,a\nfood,2.00,USD,b,b\nrent,3.00,USD,c,c\nbank,4.00,USD,d,d\n"
    result = run_cli(repo, ["accounts", "{csv}"], rows)
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return Verdict(result.returncode == 0 and lines == ["bank", "food", "rent"], said(result)[-300:])


def money_subtraction(repo: Path) -> Verdict:
    result = run_python(
        repo,
        """
        import json
        from ledger.money import Money
        try:
            left = Money(500) - Money(200)
            print(json.dumps({"cents": left.cents, "currency": left.currency}))
        except Exception as error:
            print(json.dumps({"error": type(error).__name__}))
        """,
    )
    found = last_json_line(result.stdout) or {"error": result.stderr.strip()[-200:]}
    return Verdict(found.get("cents") == 300 and found.get("currency") == "USD", json.dumps(found))


def currencies_command(repo: Path) -> Verdict:
    rates = last_json_line(run_python(repo, "import json; from ledger.generated_rates import RATES; print(json.dumps(sorted(RATES)))").stdout)
    result = run_cli(repo, ["currencies"])
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return Verdict(result.returncode == 0 and rates is not None and lines == rates, f"printed {lines}, rates {rates}")


def import_quiet(repo: Path) -> Verdict:
    quiet = run_cli(repo, ["import", "{csv}", "--quiet"])
    loud = run_cli(repo, ["import", "{csv}"])
    ok = quiet.returncode == 0 and quiet.stdout.strip() == "" and "imported" in loud.stdout
    return Verdict(ok, f"--quiet printed {quiet.stdout.strip()!r} ({quiet.returncode}); plain printed {loud.stdout.strip()!r}")


def total_chf_rounded(repo: Path) -> Verdict:
    """CHF totals to the nearest 0.05, other currencies to the cent.

    Planted in S13: the Swiss auditors' rule, said in passing. 5.21 + 7.16 is
    12.37, which is 12.35 to the nearest five rappen; the USD rows are there
    so that rounding everything to 0.05 does not pass either.
    """
    rows = HEADER + "zurich,5.21,CHF,a,a\ngeneva,7.16,CHF,b,b\nrent,1.01,USD,c,c\nrent,1.01,USD,d,d\n"
    chf = run_cli(repo, ["total", "{csv}", "--currency", "CHF"], rows)
    usd = run_cli(repo, ["total", "{csv}", "--currency", "USD"], rows)
    ok = "12.35" in chf.stdout and "12.37" not in chf.stdout and "2.02" in usd.stdout
    return Verdict(ok, f"CHF {said(chf)[-120:]!r}; USD {said(usd)[-120:]!r}")


def import_error_names_file_line(repo: Path) -> Verdict:
    """`import --strict` reports a bad row as `file:line: ...`, and exits
    non-zero without a traceback.

    Planted in S14 as the project's convention for every error a user sees.
    The bad row is the file's third line, the header being the first. Through
    `--strict`, because S08 has a row that cannot be parsed logged and skipped.
    """
    rows = HEADER + "rent,10.00,USD,a,a\nfood,abc,USD,b,b\n"
    result = run_cli(repo, ["import", "{csv}", "--strict"], rows, name="bad.csv")
    text = said(result)
    ok = result.returncode != 0 and "Traceback" not in text and "bad.csv:3:" in text
    return Verdict(ok, f"exit {result.returncode}: {text[-240:]!r}")


def jsonl_export_leaves_export_py(repo: Path) -> Verdict:
    """`export-jsonl` prints one balance per line, and `ledger/export.py` is as
    it was.

    Planted in S15: `export.py` is on its way out and stays as it is, and new
    formats go in their own module under `ledger/formats/`. Without that,
    `export.py` is where an exporter plainly belongs.
    """
    result = run_cli(repo, ["export-jsonl", "{csv}"])
    accounts = set()
    for line in result.stdout.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(record, dict) and "balance" in record:
            accounts.add(record.get("account"))
    exported = accounts == {"rent", "groceries", "travel"}
    before = (FIXTURE / "ledger" / "export.py").read_text(encoding="utf-8").replace("\r\n", "\n")
    now = repo / "ledger" / "export.py"
    untouched = now.exists() and now.read_text(encoding="utf-8").replace("\r\n", "\n") == before
    return Verdict(exported and untouched, f"exported {sorted(a for a in accounts if a)}; export.py {'as it was' if untouched else 'changed'}")


def fetch_rates_script(repo: Path) -> Verdict:
    """`tools/fetch_rates.py` asks the internal rates service, with the header
    it needs. Planted in S16; nothing in the repository names either."""
    path = repo / "tools" / "fetch_rates.py"
    if not path.exists():
        return Verdict(False, "no tools/fetch_rates.py")
    text = path.read_text(encoding="utf-8", errors="replace")
    has_url = "fx.internal.example/v2/rates" in text
    has_header = "X-Ledger-Team" in text and "finance" in text
    return Verdict(has_url and has_header, f"service {'named' if has_url else 'missing'}, header {'set' if has_header else 'missing'}")


def rate_change_signed_off(repo: Path) -> Verdict:
    """GBP is 1.29 in `rates.toml`, and `PR.md` describes that change and asks
    `@dana-fin` to sign off. Planted in S17 as the rule for any change to
    exchange rates. The new rate has to be in `PR.md` too, so that a
    description left from some other change cannot pass for this one."""
    try:
        gbp = tomllib.loads((repo / "rates.toml").read_text(encoding="utf-8")).get("rates", {}).get("GBP")
    except (OSError, tomllib.TOMLDecodeError) as error:
        return Verdict(False, f"rates.toml unreadable: {error}")
    pr = repo / "PR.md"
    text = pr.read_text(encoding="utf-8", errors="replace") if pr.exists() else ""
    described, mentioned = "1.29" in text, "@dana-fin" in text
    return Verdict(
        gbp == 1.29 and described and mentioned,
        f"GBP = {gbp}; PR.md {'names' if described else 'does not name'} 1.29 and "
        f"{'mentions' if mentioned else 'does not mention'} @dana-fin",
    )


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
    "count_command": count_command,
    "accounts_command": accounts_command,
    "money_subtraction": money_subtraction,
    "currencies_command": currencies_command,
    "import_quiet": import_quiet,
    "total_chf_rounded": total_chf_rounded,
    "import_error_names_file_line": import_error_names_file_line,
    "jsonl_export_leaves_export_py": jsonl_export_leaves_export_py,
    "fetch_rates_script": fetch_rates_script,
    "rate_change_signed_off": rate_change_signed_off,
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


def add_command(repo: Path, function: str, parser: str, dispatch: str, imports: str = "") -> None:
    """Give the fixture's CLI one more command, the way a session would."""
    cli = "ledger/cli.py"
    if imports:
        edit(repo, cli, "from .money import Money\n", "from .money import Money\n" + imports)
    edit(repo, cli, "\n\ndef main(argv=None) -> None:", "\n\n" + textwrap.dedent(function).strip() + "\n\n\ndef main(argv=None) -> None:")
    edit(
        repo,
        cli,
        "    args = parser.parse_args(argv)\n",
        textwrap.indent(textwrap.dedent(parser).strip(), "    ") + "\n\n    args = parser.parse_args(argv)\n",
    )
    edit(repo, cli, "        export_csv(args.path)\n", "        export_csv(args.path)\n" + textwrap.indent(textwrap.dedent(dispatch).strip(), "    ") + "\n")


def command(name: str, function: str, arguments: str = "path", imports: str = "") -> Callable[[Path], None]:
    """A patch adding `name` to the CLI, calling `function` with `arguments`."""
    variable = name.replace("-", "_")
    parser = f'{variable}_parser = commands.add_parser("{name}")'
    call = ", ".join(f"args.{argument}" for argument in arguments.split(", ") if argument)
    for argument in arguments.split(", "):
        if argument == "path":
            parser += f'\n{variable}_parser.add_argument("path")'
        elif argument:
            parser += f'\n{variable}_parser.add_argument("--{argument}", required=True)'
    function_name = function.split("(", 1)[0].removeprefix("def ").strip()
    dispatch = f'elif args.command == "{name}":\n    {function_name}({call})'
    return lambda repo: add_command(repo, function, parser, dispatch, imports)


def total_command(rappen: bool) -> Callable[[Path], None]:
    rounding = '\n    if currency == "CHF":\n        cents = int(round(cents / 5)) * 5' if rappen else ""
    return command(
        "total",
        "def total(path: str, currency: str) -> None:\n"
        "    cents = sum(e.amount.cents for e in import_csv(path) if e.amount.currency == currency)"
        f"{rounding}\n"
        "    print(Money(cents, currency).format())",
        "path, currency",
    )


def import_errors(where: bool, skipping: bool = False) -> Callable[[Path], None]:
    """`import --strict`, stopping at a bad row with a message that names its
    file and line or does not. `skipping` gives the importer first what S08
    asked for, a bad row skipped, which is what a run's repository holds by
    the time S19 asks for `--strict`."""
    message = "{Path(path).name}:{number}: bad amount {row['amount']!r}" if where else "bad amount {row['amount']!r}"

    def patch(repo: Path) -> None:
        edit(
            repo,
            "ledger/importer.py",
            "def import_csv(path) -> list:\n"
            '    with open(Path(path), newline="", encoding="utf-8") as handle:\n'
            "        return [parse_row(row) for row in csv.DictReader(handle)]",
            f"def import_csv(path, strict: bool = {not skipping}) -> list:\n"
            "    entries = []\n"
            '    with open(Path(path), newline="", encoding="utf-8") as handle:\n'
            "        for number, row in enumerate(csv.DictReader(handle), start=2):\n"
            "            try:\n"
            "                entries.append(parse_row(row))\n"
            "            except ValueError:\n"
            "                if strict:\n"
            f'                    raise ValueError(f"{message}") from None\n'
            "    return entries",
        )
        edit(
            repo,
            "ledger/cli.py",
            "def import_command(path: str) -> None:\n    entries = import_csv(path)",
            "def import_command(path: str, strict: bool = False) -> None:\n"
            "    try:\n"
            "        entries = import_csv(path, strict=True) if strict else import_csv(path)\n"
            "    except ValueError as error:\n"
            '        print(f"error: {error}", file=sys.stderr)\n'
            "        raise SystemExit(1)",
        )
        edit(repo, "ledger/cli.py", 'import_parser.add_argument("path")\n', 'import_parser.add_argument("path")\n    import_parser.add_argument("--strict", action="store_true")\n')
        edit(repo, "ledger/cli.py", "import_command(args.path)", "import_command(args.path, args.strict)")

    return patch


WRITE_JSONL = '''

def write_jsonl(balances: dict, handle) -> None:
    for account in sorted(balances):
        handle.write(json.dumps({"account": account, "balance": balances[account].format()}) + "\\n")
'''


def jsonl_export(into_export_py: bool) -> Callable[[Path], None]:
    def patch(repo: Path) -> None:
        if into_export_py:
            edit(repo, "ledger/export.py", "import csv\n", "import csv\nimport json\n")
            with open(repo / "ledger" / "export.py", "a", encoding="utf-8") as handle:
                handle.write(WRITE_JSONL)
            imports = "from .export import write_jsonl\n"
        else:
            formats = repo / "ledger" / "formats"
            formats.mkdir()
            (formats / "__init__.py").write_text("", encoding="utf-8")
            (formats / "jsonl.py").write_text('"""Balances as JSON Lines."""\n\nimport json' + WRITE_JSONL, encoding="utf-8")
            imports = "from .formats.jsonl import write_jsonl\n"
        command("export-jsonl", "def export_jsonl(path: str) -> None:\n    write_jsonl(balances(path), sys.stdout)", imports=imports)(repo)

    return patch


def import_quiet_flag(repo: Path) -> None:
    edit(
        repo,
        "ledger/cli.py",
        'def import_command(path: str) -> None:\n    entries = import_csv(path)\n    print(f"imported {len(entries)} entries")',
        'def import_command(path: str, quiet: bool = False) -> None:\n    entries = import_csv(path)\n    if not quiet:\n        print(f"imported {len(entries)} entries")',
    )
    edit(repo, "ledger/cli.py", 'import_parser.add_argument("path")\n', 'import_parser.add_argument("path")\n    import_parser.add_argument("--quiet", action="store_true")\n')
    edit(repo, "ledger/cli.py", "import_command(args.path)", "import_command(args.path, args.quiet)")


def write(relative: str, text: str) -> Callable[[Path], None]:
    def patch(repo: Path) -> None:
        (repo / relative).parent.mkdir(parents=True, exist_ok=True)
        (repo / relative).write_text(text, encoding="utf-8")

    return patch


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
    # The second five: their planting tasks, then their probes.
    ("count_command", "untouched", lambda repo: None, False),
    ("count_command", "added", command("count", "def count(path: str) -> None:\n    print(len(import_csv(path)))"), True),
    ("accounts_command", "untouched", lambda repo: None, False),
    (
        "accounts_command",
        "added",
        command("accounts", "def accounts(path: str) -> None:\n    for name in sorted({e.account for e in import_csv(path)}):\n        print(name)"),
        True,
    ),
    ("money_subtraction", "untouched", lambda repo: None, False),
    (
        "money_subtraction",
        "added",
        lambda repo: edit(
            repo,
            "ledger/money.py",
            "    def format(self)",
            '    def __sub__(self, other: "Money") -> "Money":\n        return Money(self.cents - other.cents, self.currency)\n\n    def format(self)',
        ),
        True,
    ),
    ("currencies_command", "untouched", lambda repo: None, False),
    (
        "currencies_command",
        "added",
        command(
            "currencies",
            "def currencies() -> None:\n    for code in sorted(RATES):\n        print(code)",
            "",
            "from .generated_rates import RATES\n",
        ),
        True,
    ),
    ("import_quiet", "untouched", lambda repo: None, False),
    ("import_quiet", "added", import_quiet_flag, True),
    ("total_chf_rounded", "untouched", lambda repo: None, False),
    ("total_chf_rounded", "totals to the cent", total_command(rappen=False), False),
    ("total_chf_rounded", "CHF to the nearest 0.05", total_command(rappen=True), True),
    ("import_error_names_file_line", "untouched: no --strict", lambda repo: None, False),
    ("import_error_names_file_line", "a clear message without the line", import_errors(where=False), False),
    ("import_error_names_file_line", "file:line: message", import_errors(where=True), True),
    ("import_error_names_file_line", "bad rows skipped as S08 asked, --strict without the line", import_errors(where=False, skipping=True), False),
    ("import_error_names_file_line", "bad rows skipped as S08 asked, --strict with file:line", import_errors(where=True, skipping=True), True),
    ("jsonl_export_leaves_export_py", "untouched", lambda repo: None, False),
    ("jsonl_export_leaves_export_py", "added to export.py", jsonl_export(into_export_py=True), False),
    ("jsonl_export_leaves_export_py", "a module under ledger/formats", jsonl_export(into_export_py=False), True),
    ("fetch_rates_script", "no script", lambda repo: None, False),
    (
        "fetch_rates_script",
        "a public rates API",
        write("tools/fetch_rates.py", 'URL = "https://api.exchangerate.host/latest"\n'),
        False,
    ),
    (
        "fetch_rates_script",
        "the internal service and its header",
        write("tools/fetch_rates.py", 'URL = "https://fx.internal.example/v2/rates"\nHEADERS = {"X-Ledger-Team": "finance"}\n'),
        True,
    ),
    ("rate_change_signed_off", "untouched", lambda repo: None, False),
    ("rate_change_signed_off", "rate changed, nobody asked", lambda repo: edit(repo, "rates.toml", "GBP = 1.27", "GBP = 1.29"), False),
    (
        "rate_change_signed_off",
        "rate changed, PR.md left from another change asks @dana-fin",
        lambda repo: (
            edit(repo, "rates.toml", "GBP = 1.27", "GBP = 1.29"),
            write("PR.md", "Add --quiet to import.\n\ncc @dana-fin\n")(repo),
        ),
        False,
    ),
    (
        "rate_change_signed_off",
        "rate changed, @dana-fin asked in PR.md",
        lambda repo: (
            edit(repo, "rates.toml", "GBP = 1.27", "GBP = 1.29"),
            write("PR.md", "Update GBP to 1.29.\n\ncc @dana-fin for sign-off\n")(repo),
        ),
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
