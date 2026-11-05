//! Press feedback (FR-L32): the pure logic for press feedback, not wired into `Clickable` yet (the renderer drops `on_pointer`, compose-rust#417). The state machine, the minimum
//! visible time, the scale and the tint alpha are plain functions so they are unit-tested
//! without a toolkit. All numbers come from the `PRESS_*` constants in `theme.rs`.
//!
//! TODO(compose-rust#305): a `PressFeedback` modifier in the toolkit replaces the use of this
//! module in `components::Clickable`; the logic here is then deleted.

use super::theme::*;

/// Where a press is. Times are milliseconds on one monotonic clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PressPhase {
    Idle,
    /// The pointer is down since `since`.
    Down { since: u64 },
    /// The pointer is up, but the press is held visible until `until`.
    Releasing { until: u64 },
}

impl PressPhase {
    /// Whether the pressed look (scale and tint) is shown.
    pub fn visible(self) -> bool {
        !matches!(self, PressPhase::Idle)
    }

    /// The pointer went down.
    pub fn down(self, now: u64) -> PressPhase {
        PressPhase::Down { since: now }
    }

    /// The pointer went up, left the element, or was cancelled. If the press has been visible
    /// for less than `PRESS_HOLD_MS` it stays until it has.
    pub fn up(self, now: u64) -> PressPhase {
        match self {
            PressPhase::Down { since } => {
                let until = since + PRESS_HOLD_MS as u64;
                if now >= until {
                    PressPhase::Idle
                } else {
                    PressPhase::Releasing { until }
                }
            }
            other => other,
        }
    }

    /// The hold timer fired: the press ends if it is still the same release.
    pub fn expire(self, now: u64) -> PressPhase {
        match self {
            PressPhase::Releasing { until } if now >= until => PressPhase::Idle,
            other => other,
        }
    }

    /// Milliseconds to wait before `expire`, for a `Releasing` phase.
    pub fn wait_ms(self, now: u64) -> u64 {
        match self {
            PressPhase::Releasing { until } => until.saturating_sub(now),
            _ => 0,
        }
    }
}

/// The scale of the element: 0.975 while the press is visible, 1 otherwise and always 1 with
/// reduced motion (the tint alone then).
pub fn press_scale(phase: PressPhase, reduced_motion: bool) -> f32 {
    if phase.visible() && !reduced_motion {
        PRESS_SCALE
    } else {
        1.0
    }
}

/// How long the scale takes to reach `phase`: 180 ms in while held, 200 ms out.
pub fn scale_duration_ms(phase: PressPhase) -> u32 {
    if matches!(phase, PressPhase::Down { .. }) {
        PRESS_IN_MS
    } else {
        PRESS_OUT_MS
    }
}

/// The tint alpha for a scheme, 0 when the press is not visible.
pub fn tint_alpha(phase: PressPhase, scheme: Scheme) -> f32 {
    if !phase.visible() {
        return 0.0;
    }
    match scheme {
        Scheme::Light => PRESS_TINT_ALPHA_LIGHT,
        Scheme::Dark => PRESS_TINT_ALPHA_DARK,
    }
}

/// The tint colour as ARGB: the RGB of the text colour with the scheme's alpha (0 if idle).
pub fn tint_argb(text_argb: u32, phase: PressPhase, scheme: Scheme) -> u32 {
    let a = (tint_alpha(phase, scheme) * 255.0).round() as u32;
    (a << 24) | (text_argb & 0x00ff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-L32
    #[test]
    fn fr_l32_quick_tap_is_held_until_min_visible_time() {
        let p = PressPhase::Idle.down(1000);
        assert!(p.visible());
        let r = p.up(1030);
        assert_eq!(r, PressPhase::Releasing { until: 1120 });
        assert!(r.visible());
        assert_eq!(r.wait_ms(1030), 90);
        assert_eq!(r.expire(1119), r);
        assert_eq!(r.expire(1120), PressPhase::Idle);
    }

    #[test]
    fn fr_l32_long_press_releases_at_once() {
        assert_eq!(PressPhase::Idle.down(0).up(500), PressPhase::Idle);
        assert_eq!(PressPhase::Idle.down(0).up(120), PressPhase::Idle);
    }

    #[test]
    fn fr_l32_new_down_restarts_and_up_when_idle_is_idle() {
        let r = PressPhase::Idle.down(0).up(10);
        assert_eq!(r.down(50), PressPhase::Down { since: 50 });
        assert_eq!(PressPhase::Idle.up(5), PressPhase::Idle);
        assert_eq!(PressPhase::Idle.wait_ms(5), 0);
    }

    #[test]
    fn fr_l32_scale_and_reduced_motion() {
        let d = PressPhase::Idle.down(0);
        assert_eq!(press_scale(PressPhase::Idle, false), 1.0);
        assert_eq!(press_scale(d, false), 0.975);
        assert_eq!(press_scale(d, true), 1.0);
        assert_eq!(scale_duration_ms(d), 180);
        assert_eq!(scale_duration_ms(d.up(10)), 200);
        assert_eq!(scale_duration_ms(PressPhase::Idle), 200);
    }

    #[test]
    fn fr_l32_tint_alpha_by_scheme() {
        let d = PressPhase::Idle.down(0);
        assert_eq!(tint_alpha(d, Scheme::Light), 0.07);
        assert_eq!(tint_alpha(d, Scheme::Dark), 0.10);
        assert_eq!(tint_alpha(PressPhase::Idle, Scheme::Dark), 0.0);
        // 7 % and 10 % of 255, rounded: 0x12 and 0x1a.
        assert_eq!(tint_argb(0xff_20_30_40, d, Scheme::Light), 0x12_20_30_40);
        assert_eq!(tint_argb(0xff_20_30_40, d, Scheme::Dark), 0x1a_20_30_40);
        assert_eq!(tint_argb(0xff_20_30_40, PressPhase::Idle, Scheme::Light), 0x00_20_30_40);
    }
}
