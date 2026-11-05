#!/usr/bin/env python3
"""Balanced gateway order inside a comparative block.

A block is one independent repetition. Inside it, direct, Melis and LiteLLM
run under the same cell factors. The order is a rotation of a seeded
permutation, so across a multiple of three blocks each gateway occupies each
position equally often. Running every block of one gateway first is not this
design.
"""
from __future__ import annotations

import itertools
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


def permutation_orders(gateways, repeats, seed):
    """Every permutation of ``gateways``, ``repeats`` times, shuffled by seed.

    Twelve blocks and three gateways use repeats=2: each of the six orders
    appears twice. The seed fixes the sequence. It does not change which
    orders exist.
    """
    if repeats < 1:
        raise ValueError("repeats must be positive")
    gates = list(gateways)
    if len(gates) < 1:
        raise ValueError("gateways must be non-empty")
    if len(set(gates)) != len(gates):
        raise ValueError("gateways must be unique")
    bag = list(itertools.permutations(gates)) * int(repeats)
    rng = random.Random(seed)
    rng.shuffle(bag)
    return [tuple(order) for order in bag]


def permutation_coverage(orders, gateways, repeats):
    """True when each permutation appears exactly ``repeats`` times."""
    expected = {tuple(p) for p in itertools.permutations(gateways)}
    counts = {}
    for order in orders:
        counts[order] = counts.get(order, 0) + 1
    return set(counts) == expected and all(v == repeats for v in counts.values())


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
