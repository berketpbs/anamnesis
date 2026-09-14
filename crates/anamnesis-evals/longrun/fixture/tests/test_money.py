import unittest

from ledger.money import Money


class MoneyTest(unittest.TestCase):
    def test_adds_cents(self):
        self.assertEqual(Money(150) + Money(250), Money(400))

    def test_formats_positive(self):
        self.assertEqual(Money(1234).format(), "12.34 USD")

    def test_formats_negative(self):
        self.assertEqual(Money(-150).format(), "-1.50 USD")

    def test_formats_small_negative(self):
        self.assertEqual(Money(-5, "EUR").format(), "-0.05 EUR")
