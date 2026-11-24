# Gaze tracking (Phase 3, optional)

Use where you're looking to resolve "this/that/here" when several windows are
open. Only ship it if it clearly improves on the active window alone.

## What the research says

- Open-source webcam gaze trackers are off by about **2–5° of visual angle** (best recent ~2°). At 60 cm that's 2–5 cm on screen: enough to pick a window or pane, not a line of text.
- Gaze-guided assistants have been studied for AR glasses (GazePointAR, GazeGPT); users rated gaze pointing as more natural. Nobody has built it for a desktop.

## Approach

- A separate `vivus-gaze` process: camera access stays isolated and can be turned off completely.
- MediaPipe face and iris landmarks plus a small per-user calibration model. Evaluate WebEyeTrack and EyeTheia.
- Calibrate silently from mouse clicks (you're almost always looking where you click) to correct drift.
- Keep a 2–3 s fixation history and align it with when you said "this/that"; turn it into a ranked list of candidate windows using geometry from the Phase 2 protocol.
- Hardware: a 1080p webcam at 30 fps or better, centred on top of the monitor, even lighting. IR cameras or Tobii are the upgrade path.

**Done when:** in a blind A/B test with several windows open, gaze plus the
active window resolves "this" correctly noticeably more often than the active
window alone.

## References

[GazePointAR](https://arxiv.org/pdf/2404.08213),
[GazeGPT](https://computationalimaging.org/?p=6731),
[WebEyeTrack](https://arxiv.org/html/2508.19544v1),
[webcam tracker thesis (~2°)](https://digitalcommons.unl.edu/computerscidiss/255),
[UnitEye (C920 ~2.4°)](https://github.com/wgnrto/uniteye).
