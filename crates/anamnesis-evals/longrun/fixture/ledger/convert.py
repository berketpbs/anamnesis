from .generated_rates import RATES
from .money import Money


def to_usd(money: Money) -> Money:
    rate = RATES[money.currency]
    return Money(round(money.cents * rate), "USD")
