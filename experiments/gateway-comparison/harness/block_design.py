#!/usr/bin/env python3
"""Balanced gateway order inside a comparative block.

A block is one independent repetition. Inside it, direct, Melis and LiteLLM
run under the same cell factors. The order is a rotation of a seeded
permutation, so across a multiple of three blocks each gateway occupies each
position equally often. Running every block of one gateway first is not this
design.
"""
from __future__ import annotations

import random


def balanced_orders(gateways, n_blocks, seed):
    if n_blocks < 1:
        raise ValueError("n_blocks must be positive")
    if len(gateways) < 1:
        raise ValueError("gateways must be non-empty")
    rng = random.Random(seed)
    base = list(gateways)
    rng.shuffle(base)
    orders = []
    width = len(base)
    for i in range(n_blocks):
        shift = i % width
        orders.append(tuple(base[shift:] + base[:shift]))
    return orders


def positions_balanced(orders):
    """True when every gateway appears equally often in every position."""
    if not orders:
        return False
    width = len(orders[0])
    if any(len(order) != width for order in orders):
        return False
    if len(orders) % width != 0:
        return False
    expect = len(orders) // width
    for pos in range(width):
        counts = {}
        for order in orders:
            counts[order[pos]] = counts.get(order[pos], 0) + 1
        if set(counts) != set(orders[0]) or any(v != expect for v in counts.values()):
            return False
    return True
