//! Formula geometry is prepared by TextView's background Markdown parser.
//! GPUI's SVG mask inherits text color; its geometry follows the surrounding
//! font and carries the real baseline into native text selection.
use gpui_kit::component::{
    ActiveTheme,
    text::{
        InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
        markdown_ast,
    },
};
use gpui_kit::*;
use ratex_layout::LayoutOptions;
use ratex_types::math_style::MathStyle;

pub(crate) struct MathPlugin {
    display: bool,
}

impl MathPlugin {
    pub(crate) fn inline() -> Self {
        Self { display: false }
    }

    pub(crate) fn display() -> Self {
        Self { display: true }
    }
}
struct Formula {
    svg: Vec<u8>,
    width: f32,
    height: f32,
    baseline: f32,
}

impl Formula {
    /// Bound expensive layout work on model-supplied expressions.
    fn parse(source: &str, display: bool) -> Option<Self> {
        if source.len() > 8_192 {
            return None;
        }
        let ast = ratex_parser::parse(source).ok()?;
        let layout = ratex_layout::layout(
            &ast,
            &LayoutOptions::default().with_style(if display {
                MathStyle::Display
            } else {
                MathStyle::Text
            }),
        );
        let list = ratex_layout::to_display_list(&layout);
        // 40 SVG units per em is a vector resource scale, not UI geometry.
        let svg = ratex_svg::render_to_svg_with_color_syntax(
            &list,
            &ratex_svg::SvgOptions {
                font_size: 40.,
                padding: 0.,
                embed_glyphs: true,
                ..Default::default()
            },
            ratex_svg::SvgColorSyntax::Rgb,
        );
        // Ratex exports its user-unit dimensions in pt. Measure at 72 DPI so
        // one pt stays one SVG unit before writing a unitless cropped viewport.
        // The default 96 DPI would inflate the viewport without scaling paths.
        let tree = resvg::usvg::Tree::from_str(
            &svg,
            &resvg::usvg::Options {
                dpi: 72.,
                ..Default::default()
            },
        )
        .ok()?;
        let bounds = tree.root().abs_bounding_box();
        if bounds.width() <= 0. || bounds.height() <= 0. {
            return None;
        }
        let body = svg.get(svg.find('>')? + 1..)?;
        let cropped = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"{} {} \
                {} {}\">{body}",
            bounds.width(),
            bounds.height(),
            bounds.x(),
            bounds.y(),
            bounds.width(),
            bounds.height()
        );
        Some(Self {
            svg: cropped.into_bytes(),
            width: bounds.width() / 40.,
            height: bounds.height() / 40.,
            baseline: (list.height as f32 * 40. - bounds.y()) / 40.,
        })
    }
}

impl MarkdownPlugin for MathPlugin {
    fn is_block(&self) -> bool {
        self.display
    }

    fn name(&self) -> &str {
        if self.display {
            "orbit-math-block"
        } else {
            "orbit-math-inline"
        }
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        context: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let source = match node {
            markdown_ast::Node::Math(value) if self.display => &value.value,
            markdown_ast::Node::InlineMath(value) if !self.display => &value.value,
            _ => return None,
        };
        let formula = Formula::parse(source, self.display)?;
        Some(
            MarkdownNode::new(self.name().to_string(), formula)
                .text(source.clone())
                .markdown(
                    context
                        .node_source(node)
                        .unwrap_or(source),
                ),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let formula = node
            .data::<Formula>()
            .expect("formula node");
        svg()
            .data(&formula.svg)
            .w(rems(formula.width))
            .h(rems(formula.height))
            .text_color(cx.theme().foreground)
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        window: &mut Window,
        _: &mut App,
    ) -> Option<InlineElement> {
        let formula = node.data::<Formula>()?;
        let font_size = context.font_size();
        let line = window.text_system().shape_line(
            "Mg".into(),
            font_size,
            &[context.text_style().to_run(2)],
            None,
        );
        let line_height = context
            .line_height()
            .max(line.ascent + line.descent);
        let text_baseline = (line_height - line.ascent - line.descent) / 2. + line.ascent;
        let formula_baseline = font_size * formula.baseline;
        let baseline = text_baseline.max(formula_baseline);
        let descent =
            (line_height - text_baseline).max(font_size * (formula.height - formula.baseline));
        Some(
            InlineElement::new(
                // Reserve the same line box as adjacent text for continuous
                // selection, while keeping the horizontal extent at ink width.
                div()
                    .relative()
                    .w(font_size * formula.width)
                    .h(baseline + descent)
                    .child(
                        svg()
                            .absolute()
                            .top(baseline - formula_baseline)
                            .data(&formula.svg)
                            .w(font_size * formula.width)
                            .h(font_size * formula.height)
                            .text_color(context.text_style().color),
                    ),
            )
            .with_baseline(baseline),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Formula;
    #[test]
    fn formulas_use_ink_bounds_and_keep_a_real_baseline() {
        let plain = Formula::parse("q=7", false).unwrap();
        let padded = Formula::parse("q=7\\quad", false).unwrap();
        assert!((plain.width - padded.width).abs() < 0.001);
        let fraction = Formula::parse("\\frac{a}{b}", true).unwrap();
        assert!(fraction.height > plain.height);
        assert!(fraction.baseline > 0. && fraction.baseline < fraction.height);
        assert!(Formula::parse(&"x".repeat(8_193), false).is_none());
    }

    #[test]
    fn inline_formula_viewport_matches_its_visible_ink() {
        let formula = Formula::parse("R_{set}=4", false).unwrap();
        let tree = resvg::usvg::Tree::from_data(&formula.svg, &Default::default()).unwrap();
        let size = tree.size().to_int_size();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height()).unwrap();
        resvg::render(&tree, Default::default(), &mut pixmap.as_mut());
        let right = pixmap
            .pixels()
            .iter()
            .enumerate()
            .filter(|(_, pixel)| pixel.alpha() != 0)
            .map(|(index, _)| index as u32 % size.width())
            .max()
            .unwrap();
        assert!(
            size.width() - right <= 2,
            "viewport width {}, last ink column {right}",
            size.width()
        );
    }
}
