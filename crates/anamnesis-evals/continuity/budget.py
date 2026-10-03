"""Persist Gemini reservations before dispatch; unknown usage keeps its hold.

This is the accounting boundary for the forthcoming client adapters, not an
account-wide cloud spending limit. No network call is made by this module.
"""
import datetime as dt
import decimal
import json
import sqlite3
import uuid
from pathlib import Path

MICRO = decimal.Decimal(1_000_000)
CAP = 50_000_000


def count(value):
    if type(value) is not int or value < 0:
        raise ValueError("token count must be a nonnegative integer, including thinking tokens")
    return value


class Budget:
    def __init__(self, path: Path, pricing: dict):
        self.path = path
        self.pricing = pricing
        for name in ("input_usd_per_million", "output_usd_per_million"):
            rate = decimal.Decimal(pricing[name])
            if not rate.is_finite() or rate <= 0:
                raise ValueError("price must be finite and positive")
        if pricing.get("includes_thinking") is not True or pricing.get("service_tier") != "standard":
            raise ValueError("only verified standard text pricing, including thinking, is supported")
        self.conn = sqlite3.connect(path, timeout=10)
        self.conn.execute("CREATE TABLE IF NOT EXISTS config (id INTEGER PRIMARY KEY, pricing TEXT NOT NULL)")
        self.conn.execute("""CREATE TABLE IF NOT EXISTS requests (
            id TEXT PRIMARY KEY, purpose TEXT NOT NULL, reserved INTEGER NOT NULL,
            charged INTEGER, usage TEXT)""")
        pinned = json.dumps(pricing, sort_keys=True)
        with self.conn:
            self.conn.execute("INSERT OR IGNORE INTO config VALUES (1, ?)", [pinned])
        if self.conn.execute("SELECT pricing FROM config WHERE id = 1").fetchone()[0] != pinned:
            self.conn.close()
            raise ValueError("pricing/model changed inside one budget; keep the existing ledger")

    def close(self):
        self.conn.close()

    def amount(self, input_tokens, output_tokens):
        # One micro-dollar per (token * dollars-per-million). Always round up.
        value = (decimal.Decimal(count(input_tokens)) * decimal.Decimal(self.pricing["input_usd_per_million"])
                 + decimal.Decimal(count(output_tokens)) * decimal.Decimal(self.pricing["output_usd_per_million"]))
        return int(value.to_integral_value(rounding=decimal.ROUND_CEILING))

    def reserve(self, model, max_input, max_output, purpose, *, today=None):
        if model != self.pricing["model"]:
            raise ValueError("unpriced model or fallback; dispatch refused")
        if (today or dt.date.today()) > dt.date.fromisoformat(self.pricing["expires_on"]):
            raise ValueError("pricing snapshot expired; re-verify before any API dispatch")
        if purpose not in ("access-check", "preparation", "summary", "brief", "retry"):
            raise ValueError("every request needs a recognized cost purpose")
        amount = self.amount(max_input, max_output)
        if amount <= 0:
            raise ValueError("reservation must bound a nonempty request")
        identity = str(uuid.uuid4())
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            if self.conn.execute("SELECT EXISTS (SELECT 1 FROM requests WHERE charged > reserved)").fetchone()[0]:
                raise ValueError("an earlier request exceeded its bound; dispatch remains stopped")
            used = self.conn.execute("SELECT coalesce(sum(coalesce(charged, reserved)), 0) FROM requests").fetchone()[0]
            if used + amount > CAP:
                raise ValueError("50 USD experiment cap reached; preserve progress and stop dispatch")
            self.conn.execute("INSERT INTO requests (id, purpose, reserved) VALUES (?, ?, ?)", [identity, purpose, amount])
            self.conn.commit()
        except BaseException:
            self.conn.rollback()
            raise
        return identity

    def settle(self, identity, model, input_tokens, output_tokens):
        if model != self.pricing["model"]:
            raise ValueError("response model differs from the reservation; retain its hold")
        charged = self.amount(input_tokens, output_tokens)
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            row = self.conn.execute("SELECT reserved, charged FROM requests WHERE id = ?", [identity]).fetchone()
            if row is None or row[1] is not None:
                raise ValueError("unknown or already settled reservation")
            self.conn.execute("UPDATE requests SET charged = ?, usage = ? WHERE id = ?",
                [charged, json.dumps(dict(input_tokens=input_tokens, output_tokens_including_thinking=output_tokens)), identity])
            self.conn.commit()
        except BaseException:
            self.conn.rollback()
            raise
        if charged > row[0]:
            # Record the actual bill even when the adapter's bound was wrong.
            # The caller must stop; lying about a bill to satisfy a cap is worse.
            raise ValueError("usage exceeded its reserved bound; actual cost recorded, stop dispatch")

    def report(self):
        known, held, total = self.conn.execute("""SELECT
            coalesce(sum(charged), 0),
            coalesce(sum(CASE WHEN charged IS NULL THEN reserved ELSE 0 END), 0),
            count(*) FROM requests""").fetchone()
        return dict(cap_usd=CAP / 1_000_000, known_usd=known / 1_000_000,
                    unresolved_reserved_usd=held / 1_000_000, requests=total,
                    remaining_usd=max(0, CAP - known - held) / 1_000_000,
                    cost_complete=held == 0)
