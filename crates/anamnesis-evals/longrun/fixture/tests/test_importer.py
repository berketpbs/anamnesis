import os
import unittest
from pathlib import Path

from ledger.importer import import_csv
from ledger.money import Money

FIXTURES = Path(os.environ["LEDGER_FIXTURES"])


class ImporterTest(unittest.TestCase):
    def test_reads_every_row(self):
        entries = import_csv(FIXTURES / "entries.csv")
        self.assertEqual(len(entries), 3)

    def test_amounts_are_cents(self):
        entries = import_csv(FIXTURES / "entries.csv")
        self.assertEqual(entries[0].amount, Money(120050, "USD"))
        self.assertEqual(entries[2].amount, Money(-4599, "EUR"))
