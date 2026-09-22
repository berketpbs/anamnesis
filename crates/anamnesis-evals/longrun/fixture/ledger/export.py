"""Balances as CSV, for the spreadsheet finance keeps."""

import csv


def write_csv(balances: dict, handle) -> None:
    """One row per account, sorted, each balance formatted the way `Money` does."""
    writer = csv.writer(handle, lineterminator="\n")
    writer.writerow(["account", "balance"])
    for account in sorted(balances):
        writer.writerow([account, balances[account].format()])
