"""Synthesize an original soundtrack that follows the promo's timeline.

usage: python3 music.py timeline.js out.wav

Everything is generated here (no samples): additive pads, a sub bass, a pluck
arpeggio, synthesized drums and risers. The arrangement follows the chapters,
and one-shot sounds land on the agent's clicks, the applied live edit, and the
two rejected edits. prep.py has already put every chapter cut on the beat grid.
"""
import bisect, json, sys
import numpy as np

SR = 48_000
T = json.loads(open(sys.argv[1]).read()[len("window.TL = "):-1])
BPM, FPS, INTRO = T["bpm"], T["fps"], T["intro"]
BEAT = 60 / BPM
DUR = T["total_frames"] / FPS
N = int(DUR * SR) + SR
rng = np.random.default_rng(7)
SM = T["session_map"]


def out_time(s):
    return INTRO + bisect.bisect_left(SM, s) / FPS


def beat_of(t):
    return t / BEAT


chapter_beats = {c["n"]: round(beat_of(out_time(c["s"]))) for c in T["chapters"]}
END_BEAT = round(beat_of(INTRO + len(SM) / FPS))
TOTAL_BEATS = int(DUR / BEAT) + 1

# section of a beat: 0 intro, -1 "meet ember", chapter numbers, 99 outro
def section(b):
    if b < 16:
        return 0
    if b >= END_BEAT:
        return 99
    cur = -1
    for n, start in chapter_beats.items():
        if b >= start:
            cur = n
    return cur


def hz(midi):
    return 440.0 * 2 ** ((midi - 69) / 12)


# D minor: Dm9 · Bbmaj7 · Fmaj7 · C(add9), two bars each
CHORDS = [[50, 53, 57, 60, 64], [46, 50, 53, 57, 62], [41, 48, 53, 57, 60], [48, 52, 55, 62, 64]]
DARK = [[50, 53, 57, 60, 63], [51, 55, 58, 62, 65], [50, 53, 57, 60, 63], [49, 52, 56, 61, 64]]
FINAL = [50, 53, 57, 60, 64, 69]


def chord_at(b):
    seq = DARK if section(b) == 3 else CHORDS
    return seq[(b // 8) % 4]


L = np.zeros(N)
R = np.zeros(N)


def add(sig, t, gain=1.0, pan=0.0):
    i = int(t * SR)
    if i >= N or i + len(sig) <= 0:
        return
    if i < 0:
        sig, i = sig[-i:], 0
    sig = sig[: N - i]
    L[i:i + len(sig)] += sig * gain * np.sqrt(0.5 * (1 - pan))
    R[i:i + len(sig)] += sig * gain * np.sqrt(0.5 * (1 + pan))


def env(n, a, d, sustain_to=0.0):
    t = np.arange(n) / SR
    e = np.minimum(1.0, t / max(a, 1e-4))
    return e * (sustain_to + (1 - sustain_to) * np.exp(-t / d))


# ------------------------------------------------------------- instruments
def pad_note(f, length, bright, detune):
    n = int(length * SR)
    t = np.arange(n) / SR
    sig = np.zeros(n)
    for h in range(1, 2 + int(bright * 6)):
        amp = 1 / h ** 1.6
        for d in (-detune, 0, detune):
            sig += amp * np.sin(2 * np.pi * f * h * (1 + d) * t + rng.uniform(0, 6.28))
    fade = np.minimum(1, np.minimum(t / 0.35, (length - t) / 0.45))
    return sig * np.clip(fade, 0, 1) / 6


def kick():
    n = int(0.42 * SR)
    t = np.arange(n) / SR
    f = 45 + 95 * np.exp(-t / 0.035)
    body = np.sin(2 * np.pi * np.cumsum(f) / SR) * np.exp(-t / 0.16)
    click = rng.standard_normal(n) * np.exp(-t / 0.002) * 0.25
    return np.tanh(1.6 * (body + click))


def hat(open_=False):
    n = int((0.22 if open_ else 0.05) * SR)
    x = np.diff(rng.standard_normal(n + 1))
    x = np.diff(np.concatenate([[0], x]))
    return x * np.exp(-np.arange(n) / SR / (0.07 if open_ else 0.012)) * 0.18


def clap():
    n = int(0.25 * SR)
    t = np.arange(n) / SR
    x = np.diff(np.concatenate([[0], rng.standard_normal(n)]))
    e = np.exp(-t / 0.05) + 0.6 * np.exp(-np.maximum(0, t - 0.012) / 0.03) * (t > 0.012)
    return x * e * 0.22 + np.sin(2 * np.pi * 190 * t) * np.exp(-t / 0.03) * 0.25


def pluck(f, bright=1.0):
    n = int(0.5 * SR)
    t = np.arange(n) / SR
    s = np.sin(2 * np.pi * f * t) + 0.35 * bright * np.sin(4 * np.pi * f * t) + 0.12 * bright * np.sin(6 * np.pi * f * t)
    return s * env(n, 0.003, 0.11)


def bass(f, length):
    n = int(length * SR)
    t = np.arange(n) / SR
    s = np.sin(2 * np.pi * f * t) + 0.18 * np.sin(4 * np.pi * f * t)
    return s * env(n, 0.006, 0.18, 0.35) * np.clip((length - t) / 0.02, 0, 1)


def riser(length):
    n = int(length * SR)
    t = np.arange(n) / SR
    k = t / length
    noise = np.diff(np.concatenate([[0], rng.standard_normal(n)])) * (k ** 2.2) * 0.12
    sweep = np.sin(2 * np.pi * np.cumsum(300 + 1500 * k ** 2) / SR) * (k ** 3) * 0.05
    return noise + sweep


def impact():
    n = int(1.6 * SR)
    t = np.arange(n) / SR
    boom = np.sin(2 * np.pi * np.cumsum(30 + 60 * np.exp(-t / 0.08)) / SR) * np.exp(-t / 0.5)
    air = rng.standard_normal(n) * np.exp(-t / 0.25) * 0.05
    return np.tanh(1.3 * boom) * 0.7 + air


def tick():
    n = int(0.05 * SR)
    t = np.arange(n) / SR
    return np.sin(2 * np.pi * 2600 * t) * np.exp(-t / 0.008) * 0.5


def chime():
    n = int(1.8 * SR)
    t = np.arange(n) / SR
    s = sum(a * np.sin(2 * np.pi * f * t) * np.exp(-t / d)
            for f, a, d in [(hz(81), .5, .9), (hz(88), .35, .7), (hz(93), .25, .5), (hz(81) * 2.76, .12, .25)])
    return s * env(n, 0.002, 10)


def denied():
    n = int(0.45 * SR)
    t = np.arange(n) / SR
    s = np.sin(2 * np.pi * 98 * t) + np.sin(2 * np.pi * 103.8 * t) + 0.4 * np.sign(np.sin(2 * np.pi * 49 * t))
    return np.tanh(s) * env(n, 0.004, 0.16) * 0.45


# ------------------------------------------------------------- arrangement
KICK, HAT, OHAT, CLAP = kick(), hat(), hat(True), clap()
pump = np.ones(N)
for b in range(TOTAL_BEATS):
    sec = section(b)
    t0 = b * BEAT
    has_kick = sec not in (0, 5, 99)
    if has_kick:
        add(KICK, t0, 0.9)
        i = int(t0 * SR)
        k = np.arange(int(BEAT * SR)) / SR
        seg = 1 - 0.55 * np.exp(-k / 0.09)
        pump[i:i + len(seg)] = seg[: max(0, min(len(seg), N - i))]
    if sec in (1, 2, 3, 4, 6):
        add(HAT, t0 + BEAT / 2, 0.8 if sec != 1 else 0.45, pan=0.25)
    if sec in (2, 4, 6) and b % 2 == 1:
        add(CLAP, t0, 0.7, pan=-0.05)
    if sec in (4, 6) and b % 4 == 3:
        add(OHAT, t0 + BEAT / 2, 0.7, pan=-0.3)
    if sec in (2, 4, 6):
        for q in (0.25, 0.75):
            add(HAT, t0 + q * BEAT, 0.3, pan=0.45)

# pads: one voicing per two bars, brightness by section
bright_of = {0: 0.2, -1: 0.35, 1: 0.4, 2: 0.55, 3: 0.3, 4: 0.85, 5: 0.35, 6: 0.75, 99: 0.5}
pad_l, pad_r = np.zeros(N), np.zeros(N)
for b in range(0, END_BEAT, 8):
    length = 8 * BEAT + 0.5
    br = bright_of[section(b)]
    for i, m in enumerate(chord_at(b)):
        i0 = int(b * BEAT * SR)
        m += 12 if m < 52 else 0  # keep the low end for the bass
        a = pad_note(hz(m), length, br, 0.004)
        c = pad_note(hz(m), length, br, 0.006)
        e = min(N, i0 + len(a)) - i0
        pad_l[i0:i0 + e] += a[:e]
        pad_r[i0:i0 + e] += c[:e]
# outro: resolve and ring out
i0 = int(END_BEAT * BEAT * SR)
for m in FINAL:
    m += 12 if m < 52 else 0
    a = pad_note(hz(m), DUR - END_BEAT * BEAT + 0.5, 0.5, 0.004)
    e = min(N, i0 + len(a)) - i0
    pad_l[i0:i0 + e] += a[:e]
    pad_r[i0:i0 + e] += pad_note(hz(m), DUR - END_BEAT * BEAT + 0.5, 0.5, 0.006)[:e]
intro_fade = np.clip(np.arange(N) / SR / (8 * BEAT), 0, 1) ** 1.5
L += pad_l * 0.22 * pump * intro_fade
R += pad_r * 0.22 * pump * intro_fade

# bass: offbeat eighths on the chord root
for b in range(16, END_BEAT):
    sec = section(b)
    if sec in (5, 99):
        continue
    root = hz(chord_at(b)[0] - 12 if chord_at(b)[0] > 45 else chord_at(b)[0])
    for q in ((0.5,) if sec in (-1, 1, 3) else (0.0, 0.5, 0.75)):
        if q == 0.0 and sec in (2, 4, 6):
            continue
        add(bass(root, BEAT * 0.45), b * BEAT + q * BEAT, 0.5)

# arpeggio: sixteenths through the chord with a ping-pong echo
arp = np.zeros((2, N))
for b in range(8, END_BEAT + 8):
    sec = section(b)
    if sec in (-1, 1, 3, 5) and b >= 16 and b < END_BEAT:
        continue
    notes = chord_at(min(b, END_BEAT - 1))
    up = 12 if sec in (4, 6) else 0
    gain = {0: min(1, (b - 8) / 8) * 0.18, 2: 0.2, 4: 0.24, 6: 0.24, 99: max(0, 1 - (b - END_BEAT) / 8) * 0.18}.get(sec, 0.2)
    for s16 in range(4):
        m = notes[(b * 4 + s16) % len(notes)] + 12 + up
        p = pluck(hz(m), 1.0 if sec != 0 else 0.4) * gain
        i = int((b * BEAT + s16 * BEAT / 4) * SR)
        for k, (ch, g) in enumerate([(0, 1.0), (1, 0.45), (0, 0.22), (1, 0.1)]):
            j = i + int(k * 0.75 * BEAT * SR)
            e = min(N, j + len(p)) - j
            if e > 0:
                arp[ch, j:j + e] += p[:e] * g
L += arp[0] * pump
R += arp[1] * pump

# risers and impacts on the cuts
add(riser(8 * BEAT), 8 * BEAT, 1.0)
add(impact(), 16 * BEAT, 0.9)
for n, b in chapter_beats.items():
    add(riser(2 * BEAT), (b - 2) * BEAT, 0.6, pan=0.1)
    add(impact(), b * BEAT, 0.35)
add(riser(4 * BEAT), (END_BEAT - 4) * BEAT, 0.8)
add(impact(), END_BEAT * BEAT, 0.7)

# the agent's actions
for c in T["clicks"]:
    add(tick(), out_time(c["s"]), 0.5, pan=0.2)
for c in T["calls"]:
    if c["tool"] == "preview_live_document":
        if "applied" in (c["result"] or ""):
            add(chime(), out_time(c["s_end"]), 0.35, pan=-0.15)
        else:
            add(denied(), out_time(c["s_end"]), 0.7)
    if c["tool"] == "highlight_elements":
        add(tick(), out_time(c["s_end"]), 0.25, pan=-0.2)

# cheap room: a few decaying, panned reflections
dry = np.stack([L, R])
wet = np.zeros_like(dry)
for ms, g in [(37, .25), (53, .22), (79, .18), (113, .14), (167, .1), (241, .07)]:
    d = int(ms / 1000 * SR)
    wet[0, d:] += dry[1, :-d] * g
    wet[1, d:] += dry[0, :-d] * g
mix = dry + wet * 0.6

# fade out with the video, then master
total = int(DUR * SR)
mix = mix[:, :total]
fade = np.clip((DUR - np.arange(total) / SR) / 2.5, 0, 1)
mix *= fade
mix /= np.percentile(np.abs(mix), 99.9)
mix = np.tanh(mix * 0.8) / np.tanh(0.8)
mix *= 0.89 / np.max(np.abs(mix))

pcm = (mix.T * 32767).astype("<i2").tobytes()
import wave
with wave.open(sys.argv[2], "wb") as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(SR)
    w.writeframes(pcm)
print(f"{DUR:.1f}s at {BPM} BPM, chapters on beats {sorted(chapter_beats.values())}, end beat {END_BEAT}")
