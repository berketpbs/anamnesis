from dataclasses import dataclass

from .money import Money


@dataclass
class Entry:
    account: str
    amount: Money
    memo: str = ""
