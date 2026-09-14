import argparse
from collections import defaultdict

from .convert import to_usd
from .importer import import_csv
from .money import Money


def balance(path: str) -> None:
    totals = defaultdict(lambda: Money(0, "USD"))
    for entry in import_csv(path):
        totals[entry.account] = totals[entry.account] + to_usd(entry.amount)
    for account in sorted(totals):
        print(f"{account}: {totals[account].format()}")


def import_command(path: str) -> None:
    entries = import_csv(path)
    print(f"imported {len(entries)} entries")


def main(argv=None) -> None:
    parser = argparse.ArgumentParser(prog="ledger")
    commands = parser.add_subparsers(dest="command", required=True)

    balance_parser = commands.add_parser("balance", help="print each account's balance in USD")
    balance_parser.add_argument("path")

    import_parser = commands.add_parser("import", help="import a CSV file")
    import_parser.add_argument("path")

    args = parser.parse_args(argv)
    if args.command == "balance":
        balance(args.path)
    elif args.command == "import":
        import_command(args.path)


if __name__ == "__main__":
    main()
