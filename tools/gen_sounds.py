#!/usr/bin/env python3
"""Synthesize the 10 CC0 bundled sound banks.

Each bank is generated from math — filtered noise transients over decaying
sine bodies — so the output is original work releasable under CC0-1.0. The
banks share names/vibes with Clicky's original profiles but the audio is
newly synthesized here (the originals are permission-only, not
redistributable).

DSP chain per sample:
    transient = uniform noise -> one-pole lowpass(click_cut) * exp(-click_decay*t)
    body      = body_gain * sin(2*pi*f(t)*t) * exp(-body_decay*t)
                with f(t) = body_hz * (1 + 0.15*exp(-90*t))  (~15% pitch drop)
    ring      = ring_gain * sin(2*pi*ring_hz*t) * exp(-ring_decay*t)  (optional)
    tap2      = second noise transient at tap2_at (bubble-wrap double pop)
Normalize to `amp`, 2 ms fade-out, mono 48 kHz PCM16.

Usage: gen_sounds.py <sounds-root> [bank ...]
Writes <root>/<id>/press-01..06.wav per bank and merges a manifest entry for
each bank into <root>/profiles.json (non-bank entries are preserved).

Stdlib only: json, math, os, random, struct, sys, wave.
"""
import json
import math
import os
import random
import struct
import sys
import wave

SR = 48000


def _one_pole_lowpass(samples, cutoff_hz):
    """One-pole low-pass to tame harsh broadband noise."""
    dt = 1.0 / SR
    rc = 1.0 / (2 * math.pi * cutoff_hz)
    alpha = dt / (rc + dt)
    out = []
    prev = 0.0
    for s in samples:
        prev += alpha * (s - prev)
        out.append(prev)
    return out


def synth_click(
    dur,
    body_hz,
    body_decay,
    click_decay,
    click_cut,
    body_gain,
    click_gain,
    ring_hz=0.0,
    ring_gain=0.0,
    ring_decay=25.0,
    tap2_at=0.0,
    tap2_gain=0.0,
    amp=0.85,
    seed=0,
):
    """One percussive key press: LP'd-noise transient + decaying sine body."""
    rnd = random.Random(seed)
    n = int(SR * dur)
    noise = _one_pole_lowpass([rnd.uniform(-1.0, 1.0) for _ in range(n)], click_cut)
    tap2_off = int(SR * tap2_at)

    out = []
    peak = 1e-9
    for i in range(n):
        t = i / SR
        s = click_gain * noise[i] * math.exp(-click_decay * t)
        if tap2_gain > 0.0 and i >= tap2_off:
            t2 = t - tap2_at
            s += click_gain * tap2_gain * noise[i - tap2_off] * math.exp(-click_decay * t2)
        f = body_hz * (1.0 + 0.15 * math.exp(-90.0 * t))
        s += body_gain * math.sin(2 * math.pi * f * t) * math.exp(-body_decay * t)
        if ring_gain > 0.0:
            s += ring_gain * math.sin(2 * math.pi * ring_hz * t) * math.exp(-ring_decay * t)
        out.append(s)
        peak = max(peak, abs(s))

    fade = int(SR * 0.002)
    norm = amp / peak
    return [
        s * norm * ((n - i) / fade if i >= n - fade else 1.0)
        for i, s in enumerate(out)
    ]


def write_wav(path, samples):
    with wave.open(path, "w") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        frames = bytearray()
        for s in samples:
            frames += struct.pack("<h", int(max(-1.0, min(1.0, s)) * 32767))
        w.writeframes(bytes(frames))


# Ten synthesized banks, one per Clicky vibe name. All fresh parameters.
BANKS = {
    "thocky": dict(
        name="Thocky", subtitle="Warm, rounded knocks", color="C68B54",
        dur=0.05, body_hz=185.0, body_decay=65.0, click_decay=310.0,
        click_cut=5500.0, body_gain=0.62, click_gain=0.75, seed=1100,
    ),
    "marbly": dict(
        name="Marbly", subtitle="Smooth, glassy resonance", color="AD9ACE",
        dur=0.048, body_hz=220.0, body_decay=52.0, click_decay=300.0,
        click_cut=6000.0, body_gain=0.58, click_gain=0.7,
        ring_hz=1650.0, ring_gain=0.07, ring_decay=45.0, seed=1200,
    ),
    "silent": dict(
        name="Silent", subtitle="A soft, quiet touch", color="94A7AD",
        dur=0.03, body_hz=210.0, body_decay=150.0, click_decay=600.0,
        click_cut=3800.0, body_gain=0.22, click_gain=0.28, amp=0.5, seed=1300,
    ),
    "poppy": dict(
        name="Poppy", subtitle="Bright little pops", color="E8A05B",
        dur=0.035, body_hz=450.0, body_decay=135.0, click_decay=520.0,
        click_cut=8500.0, body_gain=0.4, click_gain=1.0, seed=1400,
    ),
    "clicky": dict(
        name="Clicky", subtitle="Crisp, tactile clicks", color="D3BB6F",
        dur=0.04, body_hz=340.0, body_decay=110.0, click_decay=480.0,
        click_cut=9200.0, body_gain=0.34, click_gain=1.0,
        ring_hz=2400.0, ring_gain=0.13, ring_decay=170.0, seed=1500,
    ),
    "bubble-wrap": dict(
        name="Bubble Wrap", subtitle="Playful, hollow pops", color="C493B5",
        dur=0.05, body_hz=380.0, body_decay=95.0, click_decay=420.0,
        click_cut=7000.0, body_gain=0.28, click_gain=0.95,
        tap2_gain=0.65, seed=1600,
    ),
    "clacky": dict(
        name="Clacky", subtitle="Sharp, lively taps", color="DA8276",
        dur=0.038, body_hz=300.0, body_decay=105.0, click_decay=500.0,
        click_cut=9500.0, body_gain=0.4, click_gain=1.0, seed=1700,
    ),
    "creamy": dict(
        name="Creamy", subtitle="Soft, buttery texture", color="DCCCA0",
        dur=0.05, body_hz=200.0, body_decay=68.0, click_decay=330.0,
        click_cut=4200.0, body_gain=0.55, click_gain=0.55, seed=1800,
    ),
    "deep-thock": dict(
        name="Deep Thock", subtitle="Low, resonant knocks", color="82A799",
        dur=0.09, body_hz=95.0, body_decay=30.0, click_decay=230.0,
        click_cut=4600.0, body_gain=0.75, click_gain=0.65, seed=1900,
    ),
    "office": dict(
        name="Office", subtitle="Familiar everyday typing", color="7FA1C3",
        dur=0.04, body_hz=260.0, body_decay=95.0, click_decay=430.0,
        click_cut=5200.0, body_gain=0.4, click_gain=0.7, seed=2000,
    ),
}

_META_KEYS = ("name", "subtitle", "color", "seed")


def press_params(bank, variant):
    """Seeded per-variant jitter: pitch/timing variations, same character."""
    rnd = random.Random(bank["seed"] + variant)
    p = {k: v for k, v in bank.items() if k not in _META_KEYS}
    p["body_hz"] *= rnd.uniform(0.96, 1.04)
    p["body_decay"] *= rnd.uniform(0.9, 1.1)
    p["click_decay"] *= rnd.uniform(0.9, 1.1)
    p["click_cut"] *= rnd.uniform(0.95, 1.05)
    if p.get("tap2_gain"):
        p["tap2_at"] = rnd.uniform(0.007, 0.02)  # irregular double pop
    p["seed"] = (bank["seed"] + variant) * 131 + 7  # distinct noise per variant
    return p


def manifest_entry(bank_id, bank):
    return {
        "id": bank_id,
        "name": bank["name"],
        "subtitle": bank["subtitle"],
        "color": bank["color"],
        "samples": [f"{bank_id}/press-{i:02d}.wav" for i in range(1, 7)],
        "releaseSamples": None,
        "gain": 1.0,
        "provenance": {
            "sourceKind": "synthesized",
            "generator": "tools/gen_sounds.py",
            "license": "CC0-1.0",
        },
    }


def main():
    root = sys.argv[1]
    wanted = sys.argv[2:] or list(BANKS)
    for bank_id in wanted:
        bank = BANKS[bank_id]
        out_dir = os.path.join(root, bank_id)
        os.makedirs(out_dir, exist_ok=True)
        for v in range(6):
            path = os.path.join(out_dir, f"press-{v + 1:02d}.wav")
            write_wav(path, synth_click(**press_params(bank, v)))
        print(f"bank '{bank_id}': 6 press wavs")

    manifest_path = os.path.join(root, "profiles.json")
    try:
        with open(manifest_path) as f:
            manifest = json.load(f)
    except FileNotFoundError:
        manifest = []
    kept = [e for e in manifest if e.get("id") not in BANKS]
    # Synth banks keep Clicky's ordering: vibe-named entries first.
    entries = [manifest_entry(b, BANKS[b]) for b in BANKS] + kept
    with open(manifest_path, "w") as f:
        json.dump(entries, f, indent=2)
        f.write("\n")
    print(f"{manifest_path}: {len(entries)} profiles")


if __name__ == "__main__":
    main()
