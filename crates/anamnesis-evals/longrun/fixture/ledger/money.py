from dataclasses import dataclass


@dataclass(frozen=True)
class Money:
    """An amount of one currency, in integer cents."""

    cents: int
    currency: str = "USD"

    def __add__(self, other: "Money") -> "Money":
        return Money(self.cents + other.cents, self.currency)

    def format(self) -> str:
        sign = "-" if self.cents < 0 else ""
        whole, frac = divmod(self.cents, 100)
        return f"{sign}{abs(whole)}.{frac:02d} {self.currency}"
