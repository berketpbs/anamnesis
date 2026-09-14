import unittest

from ledger.convert import to_usd
from ledger.money import Money


class ConvertTest(unittest.TestCase):
    def test_usd_is_unchanged(self):
        self.assertEqual(to_usd(Money(1000)), Money(1000))

    def test_converts_gbp(self):
        self.assertEqual(to_usd(Money(1000, "GBP")), Money(1270))
