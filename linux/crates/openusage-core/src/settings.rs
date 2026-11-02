//! Display settings the Mac app stores in `settings.plist`. The panel writes them; the daemon and
//! the CLI read them through these helpers so a bar and a tooltip agree.

use crate::dashboard;
use crate::store::{Settings, MENU_BAR_LIMIT_KEY};
use crate::widget::DisplayMode;

/// `meterStyle`: `used` shows what was spent, anything else (including a missing key) shows what is left.
pub fn display_mode(settings: &Settings) -> DisplayMode {
    match settings.string("meterStyle").as_deref() {
        Some("used") => DisplayMode::Used,
        _ => DisplayMode::Remaining,
    }
}

/// `resetDisplayMode`: `absolute` is a clock time, anything else is a relative wait.
pub fn absolute_resets(settings: &Settings) -> bool {
    settings.string("resetDisplayMode").as_deref() == Some("absolute")
}

/// `menuBarLimit`: how many limits the bar shows. See `dashboard::menu_bar_limit`.
pub fn menu_bar_limit(settings: &Settings) -> usize {
    dashboard::menu_bar_limit(settings.string(MENU_BAR_LIMIT_KEY).as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_mac_display_keys() {
        let path =
            std::env::temp_dir().join(format!("openusage-settings-{}.plist", uuid::Uuid::new_v4()));
        let settings = Settings::at(path.clone());
        assert_eq!(display_mode(&settings), DisplayMode::Remaining);
        assert!(!absolute_resets(&settings));
        assert_eq!(menu_bar_limit(&settings), 3);

        settings.set_string("meterStyle", "used").unwrap();
        settings.set_string("resetDisplayMode", "absolute").unwrap();
        settings.set_string(MENU_BAR_LIMIT_KEY, "upToFive").unwrap();
        assert_eq!(display_mode(&settings), DisplayMode::Used);
        assert!(absolute_resets(&settings));
        assert_eq!(menu_bar_limit(&settings), 5);

        settings.set_string("meterStyle", "remaining").unwrap();
        settings.set_string(MENU_BAR_LIMIT_KEY, "iconOnly").unwrap();
        assert_eq!(display_mode(&settings), DisplayMode::Remaining);
        assert_eq!(menu_bar_limit(&settings), 0);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
}
