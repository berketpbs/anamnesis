import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    rates = tomllib.loads((ROOT / "rates.toml").read_text(encoding="utf-8"))["rates"]
    lines = ["RATES = {"]
    for currency in sorted(rates):
        lines.append(f'    "{currency}": {float(rates[currency])!r},')
    lines.append("}")
    (ROOT / "ledger" / "generated_rates.py").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
