# Orbit GPUI Kit patches

Upstream: [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit), revision
`1ccf011620da24d633f7cd6d3c11ca2e97798dcf` (Apache-2.0).
`gpui-base` and `gpui-component` are vendored; other Kit crates use the same pinned revision.

Local changes:

- Inline selection shares row height and includes code padding, keeping copied text and wrapping unchanged.
- Code block headers use normal flow; capped blocks retain pinned controls.
- Inline alignment applies once per line to glyphs, backgrounds, formulas and hit testing; nowrap tables use measured widths.
- `ScrollContainment` lets children scroll, then consumes remaining wheel input before ancestor lists.
- Sheets restore escaped focus on pointer-down so the close button's Cancel action reaches the sheet.
- TextView can override local image sources while keeping built-in HTTP and data URL handling.

- Table column widths account for rem-scaled default cell padding, so larger UI fonts do not wrap text inside otherwise wide columns.
- Diff offers a single line-number lane in unified mode, including matching width measurement and source selection.

Remove each patch when the pinned upstream version includes its equivalent fix.
