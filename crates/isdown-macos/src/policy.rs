//! Pure presentation and notification policies, independent of Cocoa.
use isdown_core::{Notifications, Status};

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
