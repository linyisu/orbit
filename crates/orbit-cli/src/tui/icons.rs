//! Nerd Fonts Material Design glyphs shared by terminal views.

pub(super) const CHECK: &str = "󰄬";
pub(super) const CLOSE: &str = "󰅖";
pub(super) const ALERT_OUTLINE: &str = "󰀪";
pub(super) const CIRCLE: &str = "󰝥";
pub(super) const CIRCLE_OUTLINE: &str = "󰝦";
pub(super) const PLAY: &str = "󰐊";
pub(super) const BLOCK_HELPER: &str = "󰂭";
pub(super) const CANCEL: &str = "󰜺";
pub(super) const LOADING: &str = "󰝲";
pub(super) const CHEVRON_RIGHT: &str = "󰅂";
pub(super) const ARROW_DOWN: &str = "󰁅";
pub(super) const MENU_LEFT: &str = "󰍞";
pub(super) const RHOMBUS: &str = "󰜋";
pub(super) const CIRCLE_SMALL: &str = "󰧟";
pub(super) const STAR_FOUR_POINTS: &str = "󰫢";
pub(super) const STAR_FOUR_POINTS_OUTLINE: &str = "󰫣";
pub(super) const WEB: &str = "󰖟";
pub(super) const FOLDER_OPEN: &str = "󰝰";
pub(super) const TEST_TUBE: &str = "󰙨";
pub(super) const CLIPBOARD_TEXT_OUTLINE: &str = "󰨸";
pub(super) const PACKAGE_VARIANT_CLOSED: &str = "󰏗";
pub(super) const TARGET: &str = "󰓾";
pub(super) const LINK_VARIANT: &str = "󰌹";
pub(super) const MAGNIFY: &str = "󰍉";
pub(super) const IMAGE_OUTLINE: &str = "󰥶";
pub(super) const CHECKBOX_MARKED_OUTLINE: &str = "󰄵";
pub(super) const CHECKBOX_BLANK_OUTLINE: &str = "󰄱";
pub(super) const PLUS: &str = "󰐕";

pub(super) fn checkbox(selected: bool) -> &'static str {
    if selected {
        CHECKBOX_MARKED_OUTLINE
    } else {
        CHECKBOX_BLANK_OUTLINE
    }
}
