#!/usr/bin/env python3
"""Safe, schema-aware CSV writer for raw experiment data.

Guarantees (per PROTOCOL.md "CSVs obrigatórios"):
  * UTF-8 encoding, comma separator, ``.`` decimal, stable header order.
  * Text fields are quoted/escaped by the stdlib csv module (QUOTE_MINIMAL).
  * **Missing is distinct from zero**: ``None`` is written as an empty cell,
    never coerced to ``0`` or ``"0"``. A genuine zero must be passed as ``0``.
  * ``schema_version`` is stamped automatically if not supplied.
  * REQUIRED columns must be present and non-empty, else a clear error is raised
    *before* anything is written (no partial/corrupt rows).
  * Enum and bool columns are validated at write time.
  * Append mode writes the header exactly once (when creating the file) and
    refuses to append rows with a mismatched header (prevents silent drift).

The writer never reads or rewrites existing rows — summaries are produced by a
separate aggregation step so raw data is never overwritten.

Stdlib only.
"""
from __future__ import annotations

import csv
import os
from typing import Iterable, Mapping

from schemas import (
    BOOL,
    ENUM,
    FLOAT,
    INT,
    SCHEMA_VERSION,
    columns,
    header,
    key_columns,  # noqa: F401  (re-exported for callers)
)

_BOOL_TRUE = {"true", "1", "yes", True, 1}
_BOOL_FALSE = {"false", "0", "no", False, 0}


class SchemaError(ValueError):
    """Raised when a row violates its table schema."""


def _fmt_cell(col, value) -> str:
    """Format one value as a CSV cell string, preserving missing != zero."""
    if value is None:
        if col.required:
            raise SchemaError(f"required column '{col.name}' is missing (None)")
        return ""  # missing — never "0"
    if col.kind == BOOL:
        if value in _BOOL_TRUE:
            return "true"
        if value in _BOOL_FALSE:
            return "false"
        raise SchemaError(f"column '{col.name}' expects a boolean, got {value!r}")
    if col.kind == ENUM:
        sval = str(value)
        if sval == "":
            if col.required:
                raise SchemaError(f"required enum '{col.name}' is empty")
            return ""
        if col.enum is not None and sval not in col.enum:
            raise SchemaError(
                f"column '{col.name}' value {sval!r} not in allowed {col.enum}"
            )
        return sval
    if col.kind == INT:
        # Reject floats that are not integral to avoid silent precision loss.
        if isinstance(value, bool):
            raise SchemaError(f"column '{col.name}' expects int, got bool")
        if isinstance(value, float) and not value.is_integer():
            raise SchemaError(f"column '{col.name}' expects int, got float {value!r}")
        return str(int(value))
    if col.kind == FLOAT:
        f = float(value)
        return repr(f) if f != int(f) else f"{f:.1f}"
    # str / timestamp
    sval = str(value)
    if col.required and sval == "":
        raise SchemaError(f"required column '{col.name}' is empty")
    return sval


class CsvTableWriter:
    """Appends schema-validated rows to one logical table's CSV file."""

    def __init__(self, table: str, path: str):
        self.table = table
        self.path = path
        self.columns = columns(table)
        self.header = header(table)
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        self._verify_or_write_header()

    def _verify_or_write_header(self):
        if os.path.exists(self.path) and os.path.getsize(self.path) > 0:
            with open(self.path, newline="", encoding="utf-8") as fh:
                existing = next(csv.reader(fh), [])
            if existing != self.header:
                raise SchemaError(
                    f"existing header in {self.path} does not match schema "
                    f"'{self.table}' v{SCHEMA_VERSION}:\n  existing={existing}\n  schema={self.header}"
                )
        else:
            with open(self.path, "w", newline="", encoding="utf-8") as fh:
                csv.writer(fh).writerow(self.header)

    def write_row(self, row: Mapping):
        row = dict(row)
        row.setdefault("schema_version", SCHEMA_VERSION)
        unknown = set(row) - {c.name for c in self.columns}
        if unknown:
            raise SchemaError(f"unknown columns for '{self.table}': {sorted(unknown)}")
        cells = [_fmt_cell(col, row.get(col.name)) for col in self.columns]
        with open(self.path, "a", newline="", encoding="utf-8") as fh:
            csv.writer(fh).writerow(cells)

    def write_rows(self, rows: Iterable[Mapping]):
        for r in rows:
            self.write_row(r)


def write_table(table: str, path: str, rows: Iterable[Mapping]):
    """Convenience: create the writer and append all rows."""
    w = CsvTableWriter(table, path)
    w.write_rows(rows)
    return w
