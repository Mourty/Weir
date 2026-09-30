//! Channel mapping between a strip's layout and a bus's layout: downmixing
//! what the bus has no speaker for, upmixing onto speakers the strip has no
//! channel for, and pan.

use weir_protocol::{ChannelPosition, Downmix, DownmixMethod, Side, Upmix, GAIN_MIN_DB};

/// -3 dB: the level at which two copies of a signal add up to one.
const M3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Dolby Pro Logic II's level for a surround channel in a matrix downmix,
/// into the front on its own side.
const MATRIX_NEAR: f32 = 0.8718;
/// The same, into the front on the other side, with opposite polarity.
const MATRIX_FAR: f32 = 0.4899;

/// A downmix level in dB as the standards count them, where every 6 dB
/// halves: -3 is exactly 1/sqrt(2) and -6 exactly 1/2, as in the tables of
/// ATSC A/52. The bottom of a fader leaves the channel out.
fn mix_gain(db: f32) -> f32 {
    if db <= GAIN_MIN_DB {
        0.0
    } else {
        (db / 6.0).exp2()
    }
}

/// Compute the `n_in x n_out` coefficient matrix (row-major, `[i * n_out + o]`)
/// that sends `input` positions into `output` positions, with `pan` applied
/// (`-1` full left, `0` center, `1` full right).
///
/// Rules:
/// * identical positions map 1:1;
/// * the rest downmix as [`Downmix`] describes: the center into both fronts
///   and surrounds into the front on their side at the chosen levels, or
///   the surrounds as a matrix; the LFE only when asked. A 7.1 strip's side
///   and rear surrounds each go at -3 dB into the one pair a 5.1 bus has;
/// * the methods that play one part of a strip put just that part on the
///   front pair, and nothing is upmixed;
/// * `MONO` in goes to `FL`, `FR` and `MONO` out at unity (or to every non-LFE
///   channel when the output has none of those);
/// * a mono bus gets the stereo downmix with its two sides averaged;
/// * the speakers nothing feeds then get what [`Upmix`] asks for: the
///   center the average of the front left and right (or the mono channel);
///   each other speaker the input's nearest channel on the same side, a
///   surround if it has one, else its front one or its mono channel; with
///   passive surround, the surround speakers the strip has no channel for
///   are left for [`surround_feed_row`];
/// * pan attenuates the opposite side (balance style: center is unity).
///
/// The subwoofer is never fed from other channels here: its feed is low-passed
/// first, which a matrix cannot do. See `RtStrip::sub_feed`.
pub fn channel_matrix(
    input: &[ChannelPosition],
    output: &[ChannelPosition],
    pan: f32,
    upmix: Upmix,
    downmix: &Downmix,
) -> Vec<f32> {
    use ChannelPosition::*;
    let n_in = input.len();
    let n_out = output.len();
    if n_in == 0 || n_out == 0 {
        return vec![0.0; n_in * n_out];
    }
    if n_out == 1 && output[0] == Mono {
        // A matrix downmix's surrounds cancel out in mono.
        let method = match downmix.method {
            DownmixMethod::Matrix => DownmixMethod::Standard,
            m => m,
        };
        let stereo = fold(input, &[FL, FR], &Downmix { method, ..*downmix });
        return (0..n_in)
            .map(|i| 0.5 * (stereo[i * 2] + stereo[i * 2 + 1]))
            .collect();
    }
    let mut m = fold(input, output, downmix);
    if !downmix.method.is_part() {
        upmix_into(input, output, upmix, &mut m);
    }
    for (o, p) in output.iter().enumerate() {
        let g = pan_gain(pan, p.side());
        if g != 1.0 {
            for i in 0..n_in {
                m[i * n_out + o] *= g;
            }
        }
    }
    m
}

/// Balance: how much of `side` is left at `pan`.
fn pan_gain(pan: f32, side: Side) -> f32 {
    let pan = pan.clamp(-1.0, 1.0);
    match side {
        Side::Left if pan > 0.0 => 1.0 - pan,
        Side::Right if pan < 0.0 => 1.0 + pan,
        _ => 1.0,
    }
}

/// Whether `p` is a side or rear speaker.
fn is_surround(p: ChannelPosition) -> bool {
    use ChannelPosition::*;
    matches!(p, SL | SR | RL | RR)
}

/// The other surround on the same side: rear for side, side for rear.
fn partner(p: ChannelPosition) -> ChannelPosition {
    use ChannelPosition::*;
    match p {
        SL => RL,
        RL => SL,
        SR => RR,
        RR => SR,
        other => other,
    }
}

/// How loud surround channel `p` goes where it shares its place with its
/// partner (a 7.1 strip's side and rear on a bus with one pair): -3 dB each,
/// so the pair is as loud as one. Anything else goes at full level.
fn share(input: &[ChannelPosition], p: ChannelPosition) -> f32 {
    if is_surround(p) && input.contains(&partner(p)) {
        M3DB
    } else {
        1.0
    }
}

/// The downmix methods that play one part of a strip: that part on the
/// output's front pair (each side on its own side, the center and
/// subwoofer on both), and nothing else.
fn fold_part(
    input: &[ChannelPosition],
    output: &[ChannelPosition],
    method: DownmixMethod,
) -> Vec<f32> {
    use ChannelPosition::*;
    let n_out = output.len();
    let mut m = vec![0.0f32; input.len() * n_out];
    let idx = |p: ChannelPosition| output.iter().position(|&o| o == p);
    for (i, &pos) in input.iter().enumerate() {
        let wanted = match method {
            DownmixMethod::FrontOnly => matches!(pos, FL | FR | Mono),
            DownmixMethod::CenterOnly => pos == FC,
            DownmixMethod::LfeOnly => pos == LFE,
            _ => is_surround(pos),
        };
        if !wanted {
            continue;
        }
        let row = &mut m[i * n_out..(i + 1) * n_out];
        let k = share(input, pos);
        let targets: &[ChannelPosition] = match pos.side() {
            Side::Left => &[FL],
            Side::Right => &[FR],
            Side::Center | Side::Lfe => &[FL, FR],
        };
        for &t in targets {
            if let Some(o) = idx(t) {
                row[o] = k;
            }
        }
    }
    m
}

/// Identity where the positions match, and the downmix for the rest.
fn fold(input: &[ChannelPosition], output: &[ChannelPosition], d: &Downmix) -> Vec<f32> {
    use ChannelPosition::*;
    if d.method.is_part() {
        return fold_part(input, output, d.method);
    }
    let n_out = output.len();
    let mut m = vec![0.0f32; input.len() * n_out];
    let idx = |p: ChannelPosition| output.iter().position(|&o| o == p);
    let share = |p: ChannelPosition| share(input, p);

    let center = mix_gain(d.center_db);

    let surround = mix_gain(d.surround_db);
    for (i, &pos) in input.iter().enumerate() {
        let row = &mut m[i * n_out..(i + 1) * n_out];
        if let Some(o) = idx(pos) {
            let crowded = is_surround(pos) && idx(partner(pos)).is_none();
            row[o] = if crowded { share(pos) } else { 1.0 };
            continue;
        }
        match pos {
            Mono => {
                let mut any = false;
                for p in [FL, FR, Mono] {
                    if let Some(o) = idx(p) {
                        row[o] = 1.0;
                        any = true;
                    }
                }
                if !any {
                    for (o, p) in output.iter().enumerate() {
                        if p.side() != Side::Lfe {
                            row[o] = 1.0;
                        }
                    }
                }
            }
            FC => {
                for p in [FL, FR] {
                    if let Some(o) = idx(p) {
                        row[o] = center;
                    }
                }
            }
            SL | RL | SR | RR => {
                if let Some(o) = idx(partner(pos)) {
                    // 7.1 on 5.1, or a side-surround layout on a rear one.
                    row[o] = share(pos);
                    continue;
                }
                let k = share(pos);
                let left = pos.side() == Side::Left;
                match (d.method, idx(FL), idx(FR)) {
                    (DownmixMethod::Matrix, Some(l), Some(r)) => {
                        let (to_l, to_r) = if left {
                            (-MATRIX_NEAR, MATRIX_FAR)
                        } else {
                            (-MATRIX_FAR, MATRIX_NEAR)
                        };
                        row[l] = to_l * k;
                        row[r] = to_r * k;
                    }
                    (_, l, r) => {
                        if let Some(o) = if left { l } else { r } {
                            row[o] = surround * k;
                        }
                    }
                }
            }
            FL | FR => {
                // Output has no FL/FR (and is not mono): send to center if any.
                if let Some(o) = idx(FC) {
                    row[o] = M3DB;
                }
            }
            LFE => {
                if d.lfe {
                    match (idx(FL), idx(FR), idx(FC)) {
                        (Some(l), Some(r), _) => {
                            row[l] = M3DB;
                            row[r] = M3DB;
                        }
                        (_, _, Some(c)) => row[c] = 1.0,
                        _ => {}
                    }
                }
            }
        }
    }
    m
}

/// Fill the speakers nothing feeds yet, as `upmix` asks. See
/// [`channel_matrix`].
fn upmix_into(input: &[ChannelPosition], output: &[ChannelPosition], upmix: Upmix, m: &mut [f32]) {
    use ChannelPosition::*;
    if upmix == Upmix::Off {
        return;
    }
    let n_out = output.len();
    let find = |p: ChannelPosition| input.iter().position(|&i| i == p);
    let fed = |m: &[f32], o: usize| (0..input.len()).any(|i| m[i * n_out + o] != 0.0);

    if let Some(o) = output.iter().position(|&p| p == FC) {
        if find(FC).is_none() && !fed(m, o) {
            match (find(FL), find(FR), find(Mono)) {
                (Some(l), Some(r), _) => {
                    m[l * n_out + o] = 0.5;
                    m[r * n_out + o] = 0.5;
                }
                (_, _, Some(c)) => m[c * n_out + o] = 1.0,
                _ => {}
            }
        }
    }
    if upmix == Upmix::Center {
        return;
    }
    for (o, &p) in output.iter().enumerate() {
        if matches!(p, Mono | FC | LFE) || fed(m, o) {
            continue;
        }
        let (surround, front) = match p.side() {
            Side::Left => ([SL, RL], FL),
            Side::Right => ([SR, RR], FR),
            _ => continue,
        };
        let own = surround.iter().find_map(|&s| find(s));
        let source = match upmix {
            Upmix::AllChannelStereo => own.or_else(|| find(front)).or_else(|| find(Mono)),
            // What is left is the passive surround feed's.
            _ => own,
        };
        if let Some(i) = source {
            m[i * n_out + o] = 1.0;
        }
    }
}

/// Whether a strip with `input` channels plays a passive surround feed: it
/// asks for passive surround and has a front pair to decode, and no
/// surround channels of its own.
pub fn wants_surround_feed(input: &[ChannelPosition], upmix: Upmix) -> bool {
    use ChannelPosition::*;
    upmix == Upmix::PassiveSurround
        && input.contains(&FL)
        && input.contains(&FR)
        && !input.iter().any(|&p| is_surround(p))
}

/// The coefficients of a strip's passive surround feed in its send to a bus
/// with `output` speakers, given the rest of that send `m` from
/// [`channel_matrix`]: the feed on each surround speaker nothing else
/// plays, as it is on the left ones and inverted on the right ones, with
/// `pan` applied.
pub fn surround_feed_row(
    output: &[ChannelPosition],
    pan: f32,
    downmix: &Downmix,
    m: &[f32],
) -> Vec<f32> {
    use ChannelPosition::*;
    let n_out = output.len();
    let mut row = vec![0.0; n_out];
    if downmix.method.is_part() {
        return row;
    }
    let n_in = m.len() / n_out.max(1);
    for (o, &p) in output.iter().enumerate() {
        if (0..n_in).any(|i| m[i * n_out + o] != 0.0) {
            continue;
        }
        row[o] = match p {
            SL | RL => pan_gain(pan, Side::Left),
            SR | RR => -pan_gain(pan, Side::Right),
            _ => 0.0,
        };
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use weir_protocol::ChannelLayout;
    use ChannelPosition::*;

    fn pos(l: ChannelLayout) -> Vec<ChannelPosition> {
        l.positions()
    }

    fn approx(a: &[f32], b: &[f32]) {
        assert_eq!(a.len(), b.len(), "length");
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() < 1e-4, "{a:?} != {b:?}");
        }
    }

    /// With the standard downmix.
    fn matrix(i: &[ChannelPosition], o: &[ChannelPosition], pan: f32, up: Upmix) -> Vec<f32> {
        channel_matrix(i, o, pan, up, &Downmix::default())
    }

    fn downmix(method: DownmixMethod) -> Downmix {
        Downmix {
            method,
            ..Downmix::default()
        }
    }

    const STEREO: [ChannelPosition; 2] = [FL, FR];
    const FRONT_51: [ChannelPosition; 6] = [FL, FR, FC, LFE, RL, RR];
    const ALL_71: [ChannelPosition; 8] = [FL, FR, FC, LFE, RL, RR, SL, SR];

    /// The column of `m` for output `o`, one entry per input.
    fn column(m: &[f32], n_in: usize, n_out: usize, o: usize) -> Vec<f32> {
        (0..n_in).map(|i| m[i * n_out + o]).collect()
    }

    #[test]
    fn stereo_identity() {
        let m = matrix(&STEREO, &STEREO, 0.0, Upmix::Off);
        approx(&m, &[1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn mono_to_stereo_and_back() {
        approx(&matrix(&[Mono], &STEREO, 0.0, Upmix::Off), &[1.0, 1.0]);
        approx(&matrix(&STEREO, &[Mono], 0.0, Upmix::Off), &[0.5, 0.5]);
        approx(&matrix(&[Mono], &[Mono], 0.0, Upmix::Off), &[1.0]);
    }

    #[test]
    fn surround_downmix_to_stereo() {
        let m = matrix(&pos(ChannelLayout::Surround51), &STEREO, 0.0, Upmix::Off);
        // rows: FL FR FC LFE RL RR
        approx(
            &m,
            &[
                1.0, 0.0, 0.0, 1.0, M3DB, M3DB, 0.0, 0.0, M3DB, 0.0, 0.0, M3DB,
            ],
        );
    }

    #[test]
    fn downmix_levels_follow_the_standard_steps() {
        let d = Downmix {
            center_db: -6.0,
            surround_db: GAIN_MIN_DB,
            ..Downmix::default()
        };
        let m = channel_matrix(&FRONT_51, &STEREO, 0.0, Upmix::Off, &d);
        approx(&m[4..6], &[0.5, 0.5]); // center at exactly half
        approx(&m[8..12], &[0.0, 0.0, 0.0, 0.0]); // surrounds left out
        let d = Downmix {
            center_db: -4.5,
            ..Downmix::default()
        };
        let m = channel_matrix(&FRONT_51, &STEREO, 0.0, Upmix::Off, &d);
        approx(&m[4..6], &[0.5946, 0.5946]);
    }

    #[test]
    fn matrix_downmix_folds_surrounds_in_with_opposite_polarity() {
        let m = channel_matrix(
            &FRONT_51,
            &STEREO,
            0.0,
            Upmix::Off,
            &downmix(DownmixMethod::Matrix),
        );
        // RL and RR rows: Lt = ... - 0.872 RL - 0.490 RR, Rt = ... + 0.490 RL + 0.872 RR.
        approx(&m[8..10], &[-MATRIX_NEAR, MATRIX_FAR]);
        approx(&m[10..12], &[-MATRIX_FAR, MATRIX_NEAR]);
        // The center as for the standard method.
        approx(&m[4..6], &[M3DB, M3DB]);
        // In mono the matrix would cancel the surrounds, so mono is standard.
        let m = channel_matrix(
            &FRONT_51,
            &[Mono],
            0.0,
            Upmix::Off,
            &downmix(DownmixMethod::Matrix),
        );
        assert!(m[4] > 0.3 && m[5] > 0.3, "{m:?}");
    }

    #[test]
    fn the_lfe_is_left_out_unless_asked_for() {
        let m = matrix(&FRONT_51, &STEREO, 0.0, Upmix::Off);
        approx(&m[6..8], &[0.0, 0.0]);
        let d = Downmix {
            lfe: true,
            ..Downmix::default()
        };
        let m = channel_matrix(&FRONT_51, &STEREO, 0.0, Upmix::Off, &d);
        approx(&m[6..8], &[M3DB, M3DB]);
    }

    #[test]
    fn surround_to_mono_averages_the_stereo_downmix() {
        let m = matrix(&FRONT_51, &[Mono], 0.0, Upmix::Off);
        approx(&m, &[0.5, 0.5, M3DB, 0.0, 0.5 * M3DB, 0.5 * M3DB]);
    }

    #[test]
    fn stereo_upmix_to_surround_only_fronts() {
        let m = matrix(&STEREO, &pos(ChannelLayout::Surround51), 0.0, Upmix::Off);
        approx(
            &m,
            &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        );
    }

    #[test]
    fn seven_one_to_five_one_shares_the_surrounds_at_minus_3_db() {
        let m = matrix(&ALL_71, &FRONT_51, 0.0, Upmix::Off);
        let n_out = 6;
        let (rl, rr, sl, sr) = (4, 5, 6, 7);
        assert_eq!(m[sl * n_out + 4], M3DB); // SL -> RL
        assert_eq!(m[rl * n_out + 4], M3DB); // RL -> RL
        assert_eq!(m[sr * n_out + 5], M3DB); // SR -> RR
        assert_eq!(m[rr * n_out + 5], M3DB); // RR -> RR
        assert_eq!(m[sl * n_out], 0.0);
        // Matching layouts stay 1:1.
        let m = matrix(&ALL_71, &ALL_71, 0.0, Upmix::Off);
        assert_eq!(m[sl * 8 + 6], 1.0);
        assert_eq!(m[rl * 8 + 4], 1.0);
    }

    #[test]
    fn seven_one_to_stereo_puts_both_surround_pairs_in_at_half_each() {
        let m = matrix(&ALL_71, &STEREO, 0.0, Upmix::Off);
        // -3 dB to share the side, -3 dB for the fold.
        approx(
            &column(&m, 8, 2, 0),
            &[1.0, 0.0, M3DB, 0.0, 0.5, 0.0, 0.5, 0.0],
        );
    }

    #[test]
    fn the_part_methods_play_one_part_on_the_front_pair() {
        let part = |method, input: &[ChannelPosition]| {
            channel_matrix(
                input,
                &STEREO,
                0.0,
                Upmix::AllChannelStereo,
                &downmix(method),
            )
        };
        // Rows FL FR FC LFE RL RR, columns FL FR.
        approx(
            &part(DownmixMethod::CenterOnly, &FRONT_51),
            &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        );
        approx(
            &part(DownmixMethod::LfeOnly, &FRONT_51),
            &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        );
        approx(
            &part(DownmixMethod::FrontOnly, &FRONT_51),
            &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        );
        approx(
            &part(DownmixMethod::SurroundOnly, &FRONT_51),
            &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
        );
        // A 7.1 strip's two surround pairs share, and a stereo strip has no
        // center to play.
        let m = part(DownmixMethod::SurroundOnly, &ALL_71);
        approx(
            &column(&m, 8, 2, 0),
            &[0.0, 0.0, 0.0, 0.0, M3DB, 0.0, M3DB, 0.0],
        );
        approx(&part(DownmixMethod::CenterOnly, &STEREO), &[0.0; 4]);
        // Nothing is upmixed onto a bus playing one part.
        let m = channel_matrix(
            &STEREO,
            &FRONT_51,
            0.0,
            Upmix::AllChannelStereo,
            &downmix(DownmixMethod::FrontOnly),
        );
        approx(
            &m,
            &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        );
    }

    #[test]
    fn pan_attenuates_opposite_side() {
        approx(&matrix(&[Mono], &STEREO, -1.0, Upmix::Off), &[1.0, 0.0]);
        approx(
            &matrix(&STEREO, &STEREO, 0.5, Upmix::Off),
            &[0.5, 0.0, 0.0, 1.0],
        );
        let m = matrix(&STEREO, &FRONT_51, 1.0, Upmix::Off);
        assert_eq!(m[0], 0.0); // FL->FL attenuated fully
        assert_eq!(m[6 + 1], 1.0); // FR->FR untouched
    }

    #[test]
    fn center_fill_adds_the_average_to_the_center() {
        let m = matrix(&STEREO, &FRONT_51, 0.0, Upmix::Center);
        approx(&column(&m, 2, 6, 2), &[0.5, 0.5]);
        approx(&column(&m, 2, 6, 4), &[0.0, 0.0]);
        approx(&column(&m, 2, 6, 3), &[0.0, 0.0]);
        let m = matrix(&[Mono], &FRONT_51, 0.0, Upmix::Center);
        approx(&m, &[1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn all_channel_stereo_puts_each_side_on_every_speaker_of_that_side() {
        let m = matrix(&STEREO, &ALL_71, 0.0, Upmix::AllChannelStereo);
        // rows FL, FR; columns FL FR FC LFE RL RR SL SR
        approx(
            &m,
            &[
                1.0, 0.0, 0.5, 0.0, 1.0, 0.0, 1.0, 0.0, //
                0.0, 1.0, 0.5, 0.0, 0.0, 1.0, 0.0, 1.0,
            ],
        );
        let m = matrix(&[Mono], &ALL_71, 0.0, Upmix::AllChannelStereo);
        approx(&m, &[1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn all_channel_stereo_feeds_sides_from_rears_rather_than_fronts() {
        let m = matrix(&FRONT_51, &ALL_71, 0.0, Upmix::AllChannelStereo);
        // SL (column 6) comes from RL (row 4) only; the center stays 1:1.
        approx(&column(&m, 6, 8, 6), &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        approx(&column(&m, 6, 8, 2), &[0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn upmix_leaves_downmixes_and_matching_layouts_alone() {
        for up in Upmix::ALL {
            approx(
                &matrix(&FRONT_51, &STEREO, 0.0, up),
                &matrix(&FRONT_51, &STEREO, 0.0, Upmix::Off),
            );
            approx(&matrix(&STEREO, &[Mono], 0.0, up), &[0.5, 0.5]);
            approx(
                &matrix(&ALL_71, &ALL_71, 0.0, up),
                &matrix(&ALL_71, &ALL_71, 0.0, Upmix::Off),
            );
        }
    }

    #[test]
    fn pan_applies_to_upmixed_speakers_too() {
        let m = matrix(&STEREO, &ALL_71, -1.0, Upmix::AllChannelStereo);
        // Panned hard left: nothing on any right speaker.
        for o in [1, 5, 7] {
            approx(&column(&m, 2, 8, o), &[0.0, 0.0]);
        }
        approx(&column(&m, 2, 8, 6), &[1.0, 0.0]);
    }

    #[test]
    fn passive_surround_leaves_the_surrounds_to_the_feed() {
        let m = matrix(&STEREO, &ALL_71, 0.0, Upmix::PassiveSurround);
        // The fronts and a center fill, and nothing on the surrounds.
        approx(
            &m,
            &[
                1.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0,
            ],
        );
        let d = Downmix::default();
        // The feed: as it is on the left, inverted on the right.
        approx(
            &surround_feed_row(&ALL_71, 0.0, &d, &m),
            &[0.0, 0.0, 0.0, 0.0, 1.0, -1.0, 1.0, -1.0],
        );
        let m = matrix(&STEREO, &ALL_71, -1.0, Upmix::PassiveSurround);
        approx(
            &surround_feed_row(&ALL_71, -1.0, &d, &m),
            &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
        );
        // No surround speakers, or a bus playing one part: no feed.
        approx(
            &surround_feed_row(&STEREO, 0.0, &d, &[1.0, 0.0, 0.0, 1.0]),
            &[0.0, 0.0],
        );
        approx(
            &surround_feed_row(
                &FRONT_51,
                0.0,
                &downmix(DownmixMethod::FrontOnly),
                &[0.0; 12],
            ),
            &[0.0; 6],
        );
    }

    #[test]
    fn only_a_strip_with_a_front_pair_and_no_surrounds_decodes() {
        assert!(wants_surround_feed(&STEREO, Upmix::PassiveSurround));
        assert!(!wants_surround_feed(&STEREO, Upmix::AllChannelStereo));
        assert!(!wants_surround_feed(&[Mono], Upmix::PassiveSurround));
        assert!(!wants_surround_feed(&FRONT_51, Upmix::PassiveSurround));
        // A 5.1 strip on 7.1 plays its rears on the sides instead.
        let m = matrix(&FRONT_51, &ALL_71, 0.0, Upmix::PassiveSurround);
        approx(&column(&m, 6, 8, 6), &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    }
}
