# GPUI Kit text selection patch

Source: https://github.com/longbridge/gpui-kit, revision
`1ccf011620da24d633f7cd6d3c11ca2e97798dcf` (Apache-2.0).

Only `gpui-base` is vendored. The other Kit crates retain the same pinned
revision. The workspace manifest is trimmed to build this crate independently.

Orbit changes: inline-flow selection uses the shared row height and includes
inline code padding at selected span boundaries. This keeps mixed prose/code
selection continuous without changing text typography, wrapping or copied text.

Code blocks measure the language/actions header in normal flow instead of
reserving a fixed empty line above the code. Language labels and actions can
coexist, scale with the font, and cannot overlap the first code line. Explicitly
height-capped scrolling blocks keep their existing pinned controls.

Remove this Cargo patch once an upstream revision contains the equivalent fix.

Mixed inline flows honor the final cell's text alignment, including nowrap
cells and short final lines. Each whole line receives one translation shared
by glyphs, code backgrounds, formula objects, hit testing and selection; the
individual fragments stay left-aligned internally. Scrolling nowrap tables
stop at their measured natural width instead of spreading spare space across
columns, and still scroll horizontally when the viewport is narrower.

`ScrollContainment` wraps capture-based scrollers such as `MessageScroller`.
Child masks consume movement first; a final capture handler consumes unused
wheel input at either edge or when the content fits, before ancestor virtual
lists handle it. Orbit uses this boundary for execution details so scrolling
their thought/tool records cannot move the surrounding conversation.
