import { View } from "gpui-kit";
import { h_flex } from "gpui-base";
import { Switch } from "gpui-component";
import { isDark, setDark } from "orbit-appearance";

export default class Appearance extends View {
  render(cx) {
    return h_flex().w_full().justify_end()
      .child(new Switch("dark-theme")
        .checked(isDark())
        .on_change((checked, cx) => { setDark(checked); cx.notify(); }));
  }
}
