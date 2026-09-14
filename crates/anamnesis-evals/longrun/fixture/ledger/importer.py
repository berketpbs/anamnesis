import csv
from pathlib import Path

from .entry import Entry
from .money import Money


def parse_row(row: dict) -> Entry:
    cents = round(float(row["amount"]) * 100)
    currency = row.get("currency") or "USD"
    return Entry(account=row["account"], amount=Money(cents, currency), memo=row.get("memo") or "")


def import_csv(path) -> list:
    with open(Path(path), newline="", encoding="utf-8") as handle:
        return [parse_row(row) for row in csv.DictReader(handle)]
