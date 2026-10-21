// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Speaker turns from segmentation windows alone.
//!
//! Each window's local speakers are binarised with hysteresis; the active
//! speaker with the highest probability holds the floor frame by frame. A
//! floor change inside a window is a vote for a turn boundary on the frames
//! around it, and votes from every window covering a frame are normalised by
//! how many windows cover it. Only the changes between local speakers are
//! used.
//! [`TurnBuilder`] takes windows as they stream out of
//! [`crate::segmentation::SegmentationModel::run_windows`].

use crate::segmentation::LOCAL_SPEAKERS;

/// Hop between consecutive segmentation output frames, in seconds.
pub const FRAME_STEP_SECONDS: f64 = 0.016875;
/// Span each segmentation output frame covers, in seconds.
pub const FRAME_DURATION_SECONDS: f64 = 0.0619375;
/// Turns shorter than this are given a neighbour's label rather than their own.
pub const MIN_EMBED_SECONDS: f64 = 0.5;

const ONSET: f32 = 0.5;
const OFFSET: f32 = 0.4;
/// A change seen by a window counts for frames within this radius.
const CHANGE_RADIUS_S: f64 = 0.2;
/// Fraction of covering windows that must see a change for a boundary.
const CHANGE_FRACTION: f64 = 0.3;
/// Speech gaps shorter than this are filled.
const MIN_OFF_S: f64 = 0.2;
/// Speech regions and turns shorter than this are dropped.
const MIN_ON_S: f64 = 0.1;
/// Boundaries closer than this collapse to the stronger one.
const MIN_TURN_S: f64 = 0.3;

/// Speech turns and the stretches within them where several people talk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Turns {
    /// `(start, end)` seconds, in time order, non-overlapping.
    pub spans: Vec<(f64, f64)>,
    /// `(start, end)` seconds where two or more local speakers are active in
    /// a majority of the windows covering the frame.
    pub overlap_spans: Vec<(f64, f64)>,
}

/// Accumulates per-frame votes from streamed windows. The clip length is
/// only needed at [`Self::finish`], so windows can arrive while the audio is
/// still being recorded.
#[derive(Clone)]
pub struct TurnBuilder {
    step_frames: f64,
    frames: usize,
    windows: usize,
    cov: Vec<u32>,
    speech: Vec<u32>,
    change: Vec<u32>,
    overlap: Vec<u32>,
}

/// Turns [`TurnBuilder::finish_through`] reports end at least this long
/// before the last final frame.
pub const PROVISIONAL_MARGIN_S: f64 = 3.0;

impl TurnBuilder {
    /// `step_s` is the window step the windows are produced at.
    pub fn new(step_s: f64) -> Self {
        Self {
            step_frames: step_s / FRAME_STEP_SECONDS,
            frames: 0,
            windows: 0,
            cov: Vec::new(),
            speech: Vec::new(),
            change: Vec::new(),
            overlap: Vec::new(),
        }
    }

    /// Windows pushed so far.
    pub fn windows(&self) -> usize {
        self.windows
    }

    /// Add window `index`'s per-frame local-speaker probabilities.
    pub fn push_window(&mut self, index: usize, p: &[[f32; LOCAL_SPEAKERS]]) {
        if self.frames == 0 {
            self.frames = p.len();
        }
        let frames = self.frames.min(p.len());
        let p = &p[..frames];
        let g0 = (index as f64 * self.step_frames).round() as usize;
        if self.cov.len() < g0 + frames {
            for v in [&mut self.cov, &mut self.speech, &mut self.change, &mut self.overlap] {
                v.resize(g0 + frames, 0);
            }
        }
        let act: Vec<Vec<bool>> = (0..LOCAL_SPEAKERS).map(|s| hysteresis(p, s)).collect();
        let h = holders(p, &act);
        let radius = (CHANGE_RADIUS_S / FRAME_STEP_SECONDS).round() as usize;
        let mut mark = vec![false; frames];
        for c in change_frames(&h) {
            for m in mark.iter_mut().take((c + radius + 1).min(frames)).skip(c.saturating_sub(radius)) {
                *m = true;
            }
        }
        for f in 0..frames {
            let g = g0 + f;
            self.cov[g] += 1;
            self.speech[g] += (h[f] >= 0) as u32;
            self.change[g] += mark[f] as u32;
            self.overlap[g] += (act.iter().filter(|a| a[f]).count() >= 2) as u32;
        }
        self.windows = self.windows.max(index + 1);
    }

    /// Resolve the votes of a clip `audio_s` long into turns.
    pub fn finish(mut self, audio_s: f64) -> Turns {
        if self.frames == 0 {
            return Turns::default();
        }
        let total = (audio_s / FRAME_STEP_SECONDS).ceil() as usize + self.frames + 2;
        for v in [&mut self.cov, &mut self.speech, &mut self.change, &mut self.overlap] {
            v.resize(total, 0);
        }
        resolve(&self.cov, &self.speech, &self.change, &self.overlap, audio_s)
    }

    /// Provisional turns over the frames no later window can still vote on,
    /// keeping only turns that end [`PROVISIONAL_MARGIN_S`] before the last of
    /// them. The windows to come start at `windows()` steps, so every frame
    /// before that point is final. [`Self::finish`] can still place these
    /// turns' boundaries differently; callers use them to start work early
    /// and key it to the exact span.
    pub fn finish_through(&self) -> Turns {
        let fin = ((self.windows as f64 * self.step_frames).round() as usize).min(self.cov.len());
        if fin == 0 {
            return Turns::default();
        }
        let end_s = fin as f64 * FRAME_STEP_SECONDS;
        let mut t = resolve(
            &self.cov[..fin],
            &self.speech[..fin],
            &self.change[..fin],
            &self.overlap[..fin],
            end_s,
        );
        let keep = end_s - PROVISIONAL_MARGIN_S;
        t.spans.retain(|&(_, e)| e <= keep);
        t.overlap_spans.retain(|&(_, e)| e <= keep);
        t
    }
}

/// Resolve per-frame votes into turns; `audio_s` caps the last turn's end.
fn resolve(cov: &[u32], speech: &[u32], change: &[u32], overlap: &[u32], audio_s: f64) -> Turns {
    let total = cov.len();
    let frac = |v: &[u32], g: usize| if cov[g] == 0 { 0.0 } else { v[g] as f64 / cov[g] as f64 };
    let to_f = |s: f64| (s / FRAME_STEP_SECONDS).round() as usize;
    let t = |g: usize| g as f64 * FRAME_STEP_SECONDS + FRAME_DURATION_SECONDS / 2.0;

    // Speech: half of the covering windows; short gaps fill, blips drop.
    let mut sp: Vec<bool> = (0..total).map(|g| frac(speech, g) >= 0.5).collect();
    let rs = runs(&sp);
    for pair in rs.windows(2) {
        if pair[1].0 - pair[0].1 < to_f(MIN_OFF_S) {
            sp[pair[0].1..pair[1].0].fill(true);
        }
    }
    let regions: Vec<(usize, usize)> =
        runs(&sp).into_iter().filter(|(a, b)| b - a >= to_f(MIN_ON_S)).collect();

    // Boundaries: one per run of change score >= the fraction, at its
    // peak (middle of ties); runs closer than MIN_TURN_S keep the stronger.
    let score: Vec<f64> = (0..total).map(|g| frac(change, g)).collect();
    let above: Vec<bool> = score.iter().map(|s| *s >= CHANGE_FRACTION).collect();
    let mut kept: Vec<(usize, f64)> = Vec::new();
    for (a, b) in runs(&above) {
        let mx = score[a..b].iter().copied().fold(0.0, f64::max);
        let tied: Vec<usize> = (a..b).filter(|&g| score[g] == mx).collect();
        let c = (tied[tied.len() / 2], mx);
        match kept.last_mut() {
            Some(last) if c.0 - last.0 < to_f(MIN_TURN_S) => {
                if c.1 > last.1 {
                    *last = c;
                }
            }
            _ => kept.push(c),
        }
    }

    let mut spans = Vec::new();
    for (a, b) in &regions {
        let mut s = *a;
        for (c, _) in kept.iter().filter(|(c, _)| c > a && c < b) {
            if c - s >= to_f(MIN_ON_S) {
                spans.push((t(s), t(*c)));
            }
            s = *c;
        }
        if b - s >= to_f(MIN_ON_S) {
            spans.push((t(s), t(*b).min(audio_s)));
        }
    }

    let ov: Vec<bool> = (0..total).map(|g| cov[g] > 0 && 2 * overlap[g] > cov[g]).collect();
    let overlap_spans = runs(&ov)
        .into_iter()
        .filter(|(a, b)| b - a >= to_f(MIN_ON_S))
        .map(|(a, b)| (t(a), t(b).min(audio_s)))
        .collect();
    Turns { spans, overlap_spans }
}

/// Turns from windows collected up front; each item is one window's
/// per-frame probabilities, in window order.
pub fn build_turns<W: AsRef<[[f32; LOCAL_SPEAKERS]]>>(
    windows: impl IntoIterator<Item = W>,
    audio_s: f64,
    step_s: f64,
) -> Turns {
    let mut b = TurnBuilder::new(step_s);
    for (i, w) in windows.into_iter().enumerate() {
        b.push_window(i, w.as_ref());
    }
    b.finish(audio_s)
}

/// Labels for every turn: `embedded[i]` is turn `i`'s own label when it was
/// embedded; the rest (short or unembeddable turns) take the label of the
/// embedded turn nearest in time, ties to the longer turn. Stays `None` only
/// when no turn was embedded.
pub fn label_short_turns(turns: &[(f64, f64)], embedded: &[Option<u32>]) -> Vec<Option<u32>> {
    let labelled: Vec<usize> = (0..turns.len()).filter(|&i| embedded[i].is_some()).collect();
    let len = |j: usize| turns[j].1 - turns[j].0;
    (0..turns.len())
        .map(|i| {
            if embedded[i].is_some() {
                return embedded[i];
            }
            let gap = |j: usize| (turns[j].0 - turns[i].1).max(turns[i].0 - turns[j].1).max(0.0);
            labelled
                .iter()
                .copied()
                .min_by(|&x, &y| gap(x).total_cmp(&gap(y)).then(len(y).total_cmp(&len(x))))
                .and_then(|j| embedded[j])
        })
        .collect()
}

fn hysteresis(p: &[[f32; LOCAL_SPEAKERS]], s: usize) -> Vec<bool> {
    let mut on = false;
    p.iter()
        .map(|f| {
            if !on && f[s] >= ONSET {
                on = true;
            } else if on && f[s] < OFFSET {
                on = false;
            }
            on
        })
        .collect()
}

/// Floor holder per frame (-1 when nobody is active): the most probable
/// active local speaker.
fn holders(p: &[[f32; LOCAL_SPEAKERS]], act: &[Vec<bool>]) -> Vec<i32> {
    (0..p.len())
        .map(|f| {
            (0..LOCAL_SPEAKERS)
                .filter(|&s| act[s][f])
                .max_by(|&a, &b| p[f][a].total_cmp(&p[f][b]))
                .map_or(-1, |s| s as i32)
        })
        .collect()
}

/// Frames (window-local) where the floor passes from one local speaker to
/// another; across a pause the change sits at the pause midpoint.
fn change_frames(h: &[i32]) -> Vec<usize> {
    let mut out = Vec::new();
    let (mut last, mut last_end) = (-1i32, 0usize);
    for f in 0..h.len() {
        if h[f] >= 0 {
            if f > 0 && h[f - 1] != h[f] && last >= 0 && last != h[f] {
                out.push(if h[f - 1] == last { f } else { (last_end + f) / 2 });
            }
            last = h[f];
            last_end = f + 1;
        }
    }
    out
}

fn runs(v: &[bool]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if v[i] {
            let a = i;
            while i < v.len() && v[i] {
                i += 1;
            }
            out.push((a, i));
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP: f64 = 1.0;
    const FRAMES: usize = 589;

    /// Windows over `audio_s` for a made-up timeline of `(start, end, speaker)`
    /// with global speaker ids below three. Window `w` maps global speaker `s`
    /// to local index `(s + w) % 3`, so local identities never agree between
    /// windows.
    fn windows(audio_s: f64, truth: &[(f64, f64, usize)]) -> Vec<Vec<[f32; LOCAL_SPEAKERS]>> {
        let count = if audio_s <= 10.0 { 1 } else { (audio_s - 10.0 / 1.0).floor() as usize + 1 };
        (0..count)
            .map(|w| {
                let g0 = (w as f64 * STEP / FRAME_STEP_SECONDS).round() as usize;
                (0..FRAMES)
                    .map(|f| {
                        let time = (g0 + f) as f64 * FRAME_STEP_SECONDS + FRAME_DURATION_SECONDS / 2.0;
                        let mut p = [0.02f32; LOCAL_SPEAKERS];
                        for (a, b, s) in truth {
                            if time >= *a && time < *b {
                                p[(s + w) % 3] = 0.9;
                            }
                        }
                        p
                    })
                    .collect()
            })
            .collect()
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.1
    }

    #[test]
    fn local_indices_permuted_across_windows_still_give_one_boundary() {
        let w = windows(12.0, &[(1.0, 5.0, 0), (5.0, 9.0, 1)]);
        let t = build_turns(&w, 12.0, STEP);
        assert_eq!(t.spans.len(), 2, "{:?}", t.spans);
        assert!(near(t.spans[0].0, 1.0) && near(t.spans[0].1, 5.0), "{:?}", t.spans);
        assert!(near(t.spans[1].0, 5.0) && near(t.spans[1].1, 9.0), "{:?}", t.spans);
        assert!(t.overlap_spans.is_empty());
    }

    #[test]
    fn a_boundary_across_a_short_pause_sits_at_the_pause_midpoint() {
        let w = windows(12.0, &[(1.0, 3.0, 0), (3.15, 5.0, 1)]);
        let t = build_turns(&w, 12.0, STEP);
        assert_eq!(t.spans.len(), 2, "{:?}", t.spans);
        assert!((t.spans[0].1 - 3.075).abs() < 0.06, "{:?}", t.spans);
        assert!((t.spans[1].0 - 3.075).abs() < 0.06, "{:?}", t.spans);
    }

    #[test]
    fn boundaries_under_the_minimum_turn_collapse() {
        // Changes at 3.0 and 3.2 are 0.2 s apart.
        let w = windows(12.0, &[(1.0, 3.0, 0), (3.0, 3.2, 1), (3.2, 5.0, 2)]);
        let t = build_turns(&w, 12.0, STEP);
        assert_eq!(t.spans.len(), 2, "{:?}", t.spans);
    }

    #[test]
    fn short_gaps_in_speech_are_filled_and_long_ones_kept() {
        let w = windows(12.0, &[(1.0, 2.0, 0), (2.15, 4.0, 0)]);
        let t = build_turns(&w, 12.0, STEP);
        assert_eq!(t.spans.len(), 1, "{:?}", t.spans);
        assert!(near(t.spans[0].0, 1.0) && near(t.spans[0].1, 4.0), "{:?}", t.spans);

        let w = windows(12.0, &[(1.0, 2.0, 0), (2.4, 4.0, 0)]);
        let t = build_turns(&w, 12.0, STEP);
        assert_eq!(t.spans.len(), 2, "{:?}", t.spans);
    }

    #[test]
    fn audio_under_one_window_runs_as_a_single_window() {
        let w = windows(6.0, &[(0.5, 2.0, 0), (2.0, 5.5, 1)]);
        assert_eq!(w.len(), 1);
        let t = build_turns(&w, 6.0, STEP);
        assert_eq!(t.spans.len(), 2, "{:?}", t.spans);
        assert!(near(t.spans[0].0, 0.5) && near(t.spans[1].1, 5.5), "{:?}", t.spans);
    }

    #[test]
    fn two_active_local_speakers_in_most_windows_make_an_overlap_span() {
        let w = windows(12.0, &[(1.0, 5.0, 0), (3.0, 6.0, 1)]);
        let t = build_turns(&w, 12.0, STEP);
        assert_eq!(t.overlap_spans.len(), 1, "{:?}", t.overlap_spans);
        assert!(near(t.overlap_spans[0].0, 3.0) && near(t.overlap_spans[0].1, 5.0), "{:?}", t.overlap_spans);
    }

    #[test]
    fn streaming_and_collected_windows_agree() {
        let w = windows(12.0, &[(1.0, 5.0, 0), (5.0, 9.0, 1)]);
        let mut b = TurnBuilder::new(STEP);
        for (i, win) in w.iter().enumerate() {
            b.push_window(i, win);
        }
        assert_eq!(b.finish(12.0), build_turns(&w, 12.0, STEP));
    }

    #[test]
    fn windows_pushed_before_the_length_is_known_give_the_batch_turns() {
        let truth = [(1.0, 9.0, 0), (9.0, 20.0, 1), (21.0, 33.5, 2), (34.0, 38.0, 0)];
        let w = windows(40.0, &truth);
        let mut b = TurnBuilder::new(STEP);
        for (i, win) in w.iter().enumerate() {
            b.push_window(i, win);
        }
        assert_eq!(b.windows(), w.len());
        assert_eq!(b.finish(40.0), build_turns(&w, 40.0, STEP));
    }

    #[test]
    fn provisional_turns_stop_short_of_frames_later_windows_can_move() {
        let truth = [(1.0, 9.0, 0), (9.0, 20.0, 1), (21.0, 33.5, 2), (34.0, 38.0, 0)];
        let w = windows(40.0, &truth);
        let full = build_turns(&w, 40.0, STEP);
        let mut b = TurnBuilder::new(STEP);
        assert_eq!(b.finish_through(), Turns::default());
        // Windows 0..=24 cover up to 34 s; frames before 25 s are final.
        for (i, win) in w.iter().enumerate().take(25) {
            b.push_window(i, win);
        }
        let early = b.finish_through();
        assert!(!early.spans.is_empty());
        for &(_, e) in &early.spans {
            assert!(e <= 25.0 * STEP - PROVISIONAL_MARGIN_S + 0.05, "{:?}", early.spans);
        }
        // On a clean timeline the provisional turns are the final ones.
        assert_eq!(early.spans, full.spans[..early.spans.len()].to_vec());
    }

    #[test]
    fn no_windows_give_no_turns() {
        assert_eq!(build_turns(Vec::<Vec<[f32; 3]>>::new(), 0.0, STEP), Turns::default());
    }

    #[test]
    fn short_turns_take_the_nearest_embedded_label() {
        let turns = [(0.0, 3.0), (3.0, 3.3), (3.3, 3.5), (4.0, 7.0), (7.1, 7.3), (20.0, 23.0)];
        let embedded = [Some(0), None, None, Some(1), None, Some(0)];
        let l = label_short_turns(&turns, &embedded);
        // 3.0-3.3 touches turn 0 (gap 0) and is 0.7 s from turn 3.
        assert_eq!(l, vec![Some(0), Some(0), Some(0), Some(1), Some(1), Some(0)]);
        assert_eq!(label_short_turns(&turns[..2], &[None, None]), vec![None, None]);
    }
}
