import argparse
import sys
from collections import defaultdict

from .convert import to_usd
from .export import write_csv
from .importer import import_csv
from .money import Money


def balances(path: str) -> dict:
    totals = defaultdict(lambda: Money(0, "USD"))
    for entry in import_csv(path):
        totals[entry.account] = totals[entry.account] + to_usd(entry.amount)
    return totals


def balance(path: str) -> None:
    totals = balances(path)
    for account in sorted(totals):
        print(f"{account}: {totals[account].format()}")


def import_command(path: str) -> None:
    entries = import_csv(path)
    print(f"imported {len(entries)} entries")


def export_csv(path: str) -> None:
    write_csv(balances(path), sys.stdout)


def main(argv=None) -> None:
    parser = argparse.ArgumentParser(prog="ledger")
    commands = parser.add_subparsers(dest="command", required=True)

    balance_parser = commands.add_parser("balance", help="print each account's balance in USD")
    balance_parser.add_argument("path")

    import_parser = commands.add_parser("import", help="import a CSV file")
    import_parser.add_argument("path")

    export_parser = commands.add_parser("export-csv", help="write each account's balance as CSV")
    export_parser.add_argument("path")

    args = parser.parse_args(argv)
    if args.command == "balance":
        balance(args.path)
    elif args.command == "import":
        import_command(args.path)
    elif args.command == "export-csv":
        export_csv(args.path)


if __name__ == "__main__":
    main()
