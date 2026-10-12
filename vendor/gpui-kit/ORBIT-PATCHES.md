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
