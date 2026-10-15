//! Pure presentation and notification policies, independent of Cocoa.
use owlgood_core::{Notifications, Status};

/// Shared compact row metrics, including the status indicator overlapping the icon’s bottom-right corner.
pub(crate) struct ServiceRowLayout {
    pub name_height: f64,
    pub detail_size: f64,
    pub detail_height: f64,
    pub row_height: f64,
    pub icon_y: f64,
    pub dot_y: f64,
    pub dot_x: f64,
    pub text_y: f64,
}
impl ServiceRowLayout {
    pub fn new(a: &owlgood_core::Appearance) -> Self {
        let name_height = (a.font_size * 1.3).ceil().max(17.);
        let detail_size = (a.font_size - 2.).max(11.);
        let detail_height = (detail_size * 1.3).ceil().max(15.);
        let text_height = name_height + detail_height;
        let row_height = a
            .row_height
            .max(text_height + 8.)
            .max(if a.show_site_icons {
                a.icon_size + 8.
            } else {
                0.
            });
        let icon_y = (row_height - a.icon_size) / 2.;
        Self {
            name_height,
            detail_size,
            detail_height,
            row_height,
            icon_y,
            dot_y: icon_y - 3.,
            dot_x: a.icon_size - 3.,
            text_y: (row_height - text_height) / 2.,
        }
    }
}

pub(crate) fn status_label(status: Status) -> &'static str {
    match status {
        Status::Healthy => "Opérationnel",
        Status::Warning => "Dégradation",
        Status::Failed => "Incident",
        Status::Unknown => "Inconnu",
    }
}

pub(crate) fn rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let value = hex.strip_prefix('#')?;
    if value.len() != 6 || !value.is_ascii() {
        return None;
    }
    let number = u32::from_str_radix(value, 16).ok()?;
    Some((
        ((number >> 16) & 255) as f64 / 255.,
        ((number >> 8) & 255) as f64 / 255.,
        (number & 255) as f64 / 255.,
    ))
}

#[derive(Debug, Default)]
pub(crate) struct NotificationState {
    candidate: Status,
    count: u32,
    announced: Status,
    last_sent: Option<u64>,
}

impl NotificationState {
    pub(crate) fn observe(
        &mut self,
        status: Status,
        now: u64,
        options: &Notifications,
        enabled: bool,
    ) -> bool {
        if status == Status::Unknown {
            self.count = 0;
            self.candidate = status;
            return false;
        }
        if status == self.candidate {
            self.count = self.count.saturating_add(1);
        } else {
            self.candidate = status;
            self.count = 1;
        }
        if self.count < options.confirmation_count.max(1) || self.announced == status {
            return false;
        }
        let previous = self.announced;
        let eligible = options.enabled
            && enabled
            && match status {
                Status::Failed => true,
                Status::Warning => options.on_warning,
                Status::Healthy => {
                    options.on_recovery && matches!(previous, Status::Failed | Status::Warning)
                }
                Status::Unknown => false,
            };
        if !eligible {
            self.announced = status;
            return false;
        }
        if self
            .last_sent
            .is_some_and(|last| now.saturating_sub(last) < options.cooldown_secs)
        {
            return false;
        }
        self.announced = status;
        self.last_sent = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> Notifications {
        Notifications {
            enabled: true,
            confirmation_count: 2,
            cooldown_secs: 60,
            on_warning: true,
            on_recovery: true,
            sound: false,
        }
    }
    #[test]
    fn compact_row_overlays_dot_on_bottom_right_corner() {
        let appearance = owlgood_core::Appearance::default();
        let row = ServiceRowLayout::new(&appearance);
        assert_eq!(row.row_height, 44.);
        assert_eq!(row.text_y, 6.);
        assert_eq!(row.dot_y + 3., row.icon_y);
        assert_eq!(row.dot_x + 3., appearance.icon_size);
        assert!(row.dot_y >= 0.);
        assert!(row.icon_y + appearance.icon_size <= row.row_height);
    }

    #[test]
    fn compact_rows_fit_large_fonts_and_icons_without_overlap() {
        for font_size in [10., 13., 32.] {
            for icon_size in [12., 28., 64.] {
                for show_site_icons in [false, true] {
                    let a = owlgood_core::Appearance {
                        font_size,
                        icon_size,
                        show_site_icons,
                        row_height: 24.,
                        ..Default::default()
                    };
                    let row = ServiceRowLayout::new(&a);
                    assert!(row.text_y >= 4.);
                    assert!(
                        row.text_y + row.detail_height + row.name_height <= row.row_height - 4.
                    );
                    if show_site_icons {
                        assert!(row.dot_y >= 0.);
                        assert!(row.dot_y < row.icon_y);
                        assert!(row.dot_y + 6. > row.icon_y);
                        assert_eq!(row.dot_x + 3., icon_size);
                        assert!(row.icon_y + icon_size <= row.row_height);
                    }
                }
            }
        }
    }

    #[test]
    fn confirms_and_deduplicates() {
        let mut state = NotificationState::default();
        assert!(!state.observe(Status::Failed, 0, &options(), true));
        assert!(state.observe(Status::Failed, 30, &options(), true));
        assert!(!state.observe(Status::Failed, 90, &options(), true));
    }
    #[test]
    fn unknown_breaks_confirmation() {
        let mut state = NotificationState::default();
        for status in [Status::Failed, Status::Unknown, Status::Failed] {
            assert!(!state.observe(status, 0, &options(), true));
        }
    }
    #[test]
    fn initial_healthy_is_not_recovery() {
        let mut state = NotificationState::default();
        assert!(!state.observe(Status::Healthy, 0, &options(), true));
        assert!(!state.observe(Status::Healthy, 30, &options(), true));
    }
    #[test]
    fn recovery_waits_for_cooldown() {
        let mut state = NotificationState::default();
        state.observe(Status::Failed, 0, &options(), true);
        assert!(state.observe(Status::Failed, 30, &options(), true));
        assert!(!state.observe(Status::Healthy, 40, &options(), true));
        assert!(!state.observe(Status::Healthy, 50, &options(), true));
        assert!(state.observe(Status::Healthy, 90, &options(), true));
    }
    #[test]
    fn disabled_site_does_not_notify() {
        let mut state = NotificationState::default();
        assert!(!state.observe(Status::Failed, 0, &options(), false));
        assert!(!state.observe(Status::Failed, 90, &options(), false));
    }
    #[test]
    fn parses_colors_without_panicking_on_unicode() {
        assert_eq!(rgb("#ff8000"), Some((1., 128. / 255., 0.)));
        for value in ["ff0000", "#xxyyzz", "#é🦀", "#fff"] {
            assert_eq!(rgb(value), None);
        }
    }
    #[test]
    fn labels_distinguish_unknown_and_healthy() {
        assert_ne!(status_label(Status::Unknown), status_label(Status::Healthy));
    }
}
