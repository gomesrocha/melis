#!/usr/bin/env python3
"""Planning widths for the mean of paired block differences.

The half-width is a marginal Student-t interval. It is not a simultaneous
interval, and Holm does not turn it into one. The pilot standard deviation
is an estimate: the sensitivity column uses a chi-square upper bound for
sigma, under an iid normal model of the block differences. That bound is
not a hidden multiplier.
"""
from __future__ import annotations

import math

# Candidate half-widths. They are not approved decision criteria.
CANDIDATE_HALF_WIDTHS_MS = (0.50, 0.25)
PLANNING_N = (12, 18, 24, 30)


def _betacf(a: float, b: float, x: float) -> float:
    am, bm, az = 1.0, 1.0, 1.0
    qab, qap, qam = a + b, a + 1.0, a - 1.0
    bz = 1.0 - qab * x / qap
    for m in range(1, 201):
        em = float(m)
        tem = em + em
        d = em * (b - em) * x / ((qam + tem) * (a + tem))
        ap = az + d * am
        bp = bz + d * bm
        d = -(a + em) * (qab + em) * x / ((a + tem) * (qap + tem))
        app = ap + d * az
        bpp = bp + d * bz
        am, bm, az = ap, bp, app
        aold = az
        if abs(bpp) < 1e-30:
            bz = bpp
            continue
        am /= bpp
        bm /= bpp
        az /= bpp
        bz = 1.0
        if abs(az - aold) < 3e-14 * abs(az):
            return az
    return az


def _betai(a: float, b: float, x: float) -> float:
    if x <= 0.0:
        return 0.0
    if x >= 1.0:
        return 1.0
    log_front = math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b)
    if x < (a + 1.0) / (a + b + 2.0):
        front = math.exp(math.log(x) * a + math.log(1.0 - x) * b + log_front) / a
        return front * _betacf(a, b, x)
    front = math.exp(math.log(x) * a + math.log(1.0 - x) * b + log_front) / b
    return 1.0 - front * _betacf(b, a, 1.0 - x)


def t_ppf(probability: float, df: int) -> float:
    """Lower-tail quantile of Student's t. `probability` is the cdf, e.g. 0.975."""
    if df < 1:
        raise ValueError("df must be positive")
    lo, hi = 0.0, 100.0
    for _ in range(80):
        mid = (lo + hi) / 2.0
        # Two-sided survival via the regularized beta, then the positive cdf.
        x = df / (df + mid * mid)
        survival = _betai(df / 2.0, 0.5, x)
        cdf = 1.0 - survival / 2.0
        if cdf < probability:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2.0


def _gamma_series_p(shape: float, x: float) -> float:
    log_prefix = shape * math.log(x) - x - math.lgamma(shape)
    term = 1.0 / shape
    total = term
    for n in range(1, 400):
        term *= x / (shape + n)
        total += term
        if abs(term) < abs(total) * 1e-14:
            break
    return math.exp(log_prefix) * total


def chi2_cdf(x: float, df: int) -> float:
    """Lower-tail cdf of chi-square with `df` degrees of freedom."""
    if x <= 0.0:
        return 0.0
    # P(df/2, x/2) = lower regularized gamma.
    shape = df / 2.0
    z = x / 2.0
    if z < shape + 1.0:
        return _gamma_series_p(shape, z)
    # Q via the continued fraction of Numerical Recipes, then 1-Q.
    log_prefix = shape * math.log(z) - z - math.lgamma(shape)
    b = z + 1.0 - shape
    c = 1e30
    d = 1.0 / b
    h = d
    for i in range(1, 400):
        an = -i * (i - shape)
        b += 2.0
        d = an * d + b
        if abs(d) < 1e-30:
            d = 1e-30
        c = b + an / c
        if abs(c) < 1e-30:
            c = 1e-30
        d = 1.0 / d
        delta = d * c
        h *= delta
        if abs(delta - 1.0) < 1e-12:
            break
    upper = math.exp(log_prefix) * h
    return max(0.0, min(1.0, 1.0 - upper))


def chi2_ppf(probability: float, df: int) -> float:
    lo, hi = 0.0, max(10.0, df * 4.0)
    while chi2_cdf(hi, df) < probability:
        hi *= 2.0
    for _ in range(80):
        mid = (lo + hi) / 2.0
        if chi2_cdf(mid, df) < probability:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2.0


def marginal_half_width_ms(n_blocks: int, sigma_ms: float) -> float:
    """Half-width of a two-sided 95% t interval for the mean of n block differences."""
    if n_blocks < 2:
        raise ValueError("n_blocks must be at least 2")
    return t_ppf(0.975, n_blocks - 1) * sigma_ms / math.sqrt(n_blocks)


def sigma_upper_95_ms(sample_sd_ms: float, pilot_blocks: int) -> float:
    """One-sided 95% upper bound for sigma, chi-square, df = pilot_blocks - 1.

    Assumes the pilot block differences are iid normal. The bound is
    s * sqrt(df / chi2_{df, 0.05}).
    """
    df = pilot_blocks - 1
    if df < 1:
        raise ValueError("pilot_blocks must be at least 2")
    return sample_sd_ms * math.sqrt(df / chi2_ppf(0.05, df))


def planning_rows(*, contrast: str, sample_sd_ms: float, pilot_blocks: int,
                  observed_switch_s_per_block: float | None) -> list[dict]:
    sigma_hi = sigma_upper_95_ms(sample_sd_ms, pilot_blocks)
    rows = []
    for n in PLANNING_N:
        if n % 6 != 0:
            raise ValueError("planning N must repeat all six permutations")
        plug = marginal_half_width_ms(n, sample_sd_ms)
        sens = marginal_half_width_ms(n, sigma_hi)
        load_s = n * 3 * (20 + 60)
        switch_s = None if observed_switch_s_per_block is None else n * observed_switch_s_per_block
        row = {
            "contrast": contrast,
            "pilot_blocks": pilot_blocks,
            "sample_sd_ms": sample_sd_ms,
            "sigma_upper_95_ms": sigma_hi,
            "n_blocks": n,
            "permutation_repeats": n // 6,
            "half_width_plugin_ms": plug,
            "half_width_sigma_upper_ms": sens,
            "configured_load_s": load_s,
            "switch_overhead_s": switch_s,
            "half_width_method": "marginal_student_t_95_two_sided",
            "sigma_method": "chi_square_upper_95_iid_normal_block_differences",
            "approved": False,
        }
        for target in CANDIDATE_HALF_WIDTHS_MS:
            key = f"plugin_meets_{target:.2f}ms"
            row[key] = plug <= target
            row[f"sigma_upper_meets_{target:.2f}ms"] = sens <= target
        rows.append(row)
    return rows
