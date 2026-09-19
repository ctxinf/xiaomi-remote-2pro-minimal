#!/usr/bin/env python3
"""`start` hook: put "正在录音…" on screen the instant the key goes down.

This exists because the expensive silence in this pipeline is not the network,
it is the gap between pressing the button and having any idea whether the
gateway noticed. Without it you find out that the remote was asleep, or the
service was dead, only after you finish talking into nothing.

It is deliberately tiny — no network, no config beyond `[output]`, no
credentials. The gateway spawns it detached and never waits for it, so the cost
is one `gdbus` round trip that runs alongside the recording.

`[output.indicator.recording] show = false` is this whole script switched off; the
other two stages keep working, since nothing here is shared state.

Step 2 of 3 (`识别中…`) and step 3 (`识别文本：…`) both live in `pipeline.py`,
the `end` hook. The two processes share nothing but the popup id — see the
`Indicator` docstring in common.py for why that is enough.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import Fail, Indicator, load_config, log  # noqa: E402


def main() -> int:
    try:
        cfg = load_config(resolve_secrets=False)  # nothing here needs a key
    except Fail as e:
        log(f"[error] {e}")
        return 1

    ind = Indicator(cfg)
    if not ind.shows("recording"):
        return 0
    # Take the turn before drawing anything. If this process stalls long enough
    # for the `end` hook to claim it back, the call below is dropped rather than
    # allowed to resurrect "正在录音…" after the transcript is already out.
    ind.claim()

    # The dead-man timer for step 1 (overridable with `ttl_sec` in
    # `[output.indicator.recording]`). The `end` hook normally overwrites this
    # popup the moment the key comes up, so this only fires when `end` never
    # runs at all: a press too short to produce an audio file, a service
    # restarted mid-recording, a stream that ends in an error. 5 min is
    # comfortably longer than any real utterance, and short enough that a stuck
    # popup is an annoyance rather than something you have to go dismiss.
    ind.progress("正在录音…", stage="recording", ttl_sec=300,
                 accent=Indicator.ACCENT_RECORDING)
    return 0


if __name__ == "__main__":
    sys.exit(main())
