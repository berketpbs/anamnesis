# ledger

A small bookkeeping library with a command line.

```
python -m ledger.cli balance entries.csv
python -m ledger.cli import entries.csv
python -m ledger.cli export-csv entries.csv
```

Amounts are kept as integer cents in `Money`. Rows come from CSV files with
`account`, `amount`, `currency` and `memo` columns.
