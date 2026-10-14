//! Content-coordinate selection constrained to its starting region.
//! Timer-driven edge scrolling follows tmux: https://github.com/tmux/tmux/blob/master/window-copy.c
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::{self, Write},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    buffer::{Buffer, Cell},
    layout::Rect,
};
use unicode_width::UnicodeWidthStr;

pub(super) fn copy_text(text: &str) -> io::Result<()> {
    write!(
        io::stdout(),
        "\x1b]52;c;{}\x07",
        STANDARD.encode(text.as_bytes())
    )?;
    io::stdout().flush()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Transcript,
    Footer,
    Approval,
}

struct SelectedMedia {
    block: super::media::SelectionBlock,
    baseline: usize,
    text: std::sync::Arc<str>,
}

#[derive(Debug, Default)]
pub struct Selection {
    pub viewport: Rect,
    pub footer: Rect,
    pub approval: Rect,
    pub offset: usize,
    region: Option<Region>,
    anchor: Option<(u16, usize)>,
    end: Option<(u16, usize)>,
    pointer: Option<(u16, u16)>,
    down: Option<(u16, u16)>,
    dragging: bool,
    moved: bool,
    cleared_on_press: bool,
    extend: bool,
    rows: BTreeMap<usize, Vec<Cell>>,
    decorations: BTreeMap<usize, super::transcript::Decorations>,
    filled: Option<(usize, usize)>,
    latest: Option<Buffer>,
    frozen: Option<Buffer>,
    last_auto: Option<Instant>,
}

impl Selection {
    pub(super) fn clear(&mut self) {
        self.region = None;
        self.anchor = None;
        self.end = None;
        self.pointer = None;
        self.down = None;
        self.dragging = false;
        self.moved = false;
        self.cleared_on_press = false;
        self.extend = false;
        self.rows.clear();
        self.filled = None;
        self.frozen = None;
        self.last_auto = None;
    }

    pub fn active(&self) -> bool {
        self.anchor.is_some()
    }

    pub fn transcript_drag(&self) -> bool {
        self.dragging && self.region == Some(Region::Transcript)
    }

    pub fn dragging(&self) -> bool {
        self.dragging
    }

    fn area(&self) -> Rect {
        match self.region {
            Some(Region::Transcript) => self.viewport,
            Some(Region::Footer) => self.footer,
            Some(Region::Approval) => self.approval,
            None => Rect::default(),
        }
    }

    fn base(&self) -> usize {
        if self.region == Some(Region::Transcript) {
            self.offset
        } else {
            0
        }
    }

    fn position(&self, (x, y): (u16, u16)) -> (u16, usize) {
        let area = self.area();
        (
            x.clamp(area.x, area.right().saturating_sub(1)) - area.x,
            (y.clamp(area.y, area.bottom().saturating_sub(1)) - area.y) as usize + self.base(),
        )
    }

    pub fn click(&self, event: &Event) -> Option<(u16, u16)> {
        if let Event::Mouse(mouse) = event
            && mouse.kind == MouseEventKind::Up(MouseButton::Left)
            && self.dragging
            && !self.moved
            && !(self.cleared_on_press && self.region == Some(Region::Transcript))
            && self.down == Some((mouse.column, mouse.row))
        {
            self.down
        } else {
            None
        }
    }

    pub fn begin_scroll(&mut self) {
        self.frozen = None;

        if self.dragging && self.region == Some(Region::Transcript) {
            self.extend = true;
            self.moved = true;
        }
    }

    pub fn auto_scroll(&mut self, now: Instant) -> i8 {
        if !self.dragging || !self.moved || self.region != Some(Region::Transcript) {
            return 0;
        }
        let Some((_, y)) = self.pointer else {
            return 0;
        };

        let direction = if y <= self.viewport.y {
            -1
        } else if y >= self.viewport.bottom().saturating_sub(1) {
            1
        } else {
            return 0;
        };

        if self
            .last_auto
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(80))
        {
            return 0;
        }

        self.last_auto = Some(now);
        self.begin_scroll();

        direction
    }

    pub fn handle(&mut self, event: &Event) -> io::Result<bool> {
        match event {
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let cleared_on_press = self.active() && self.moved;
                    self.clear();
                    self.cleared_on_press = cleared_on_press;
                    let point = (mouse.column, mouse.row);
                    self.region = [
                        (Region::Transcript, self.viewport),
                        (Region::Footer, self.footer),
                        (Region::Approval, self.approval),
                    ]
                    .into_iter()
                    .find(|(_, area)| area.contains(point.into()))
                    .map(|(region, _)| region);

                    if self.region.is_some() {
                        self.anchor = Some(self.position(point));
                        self.end = self.anchor;
                        self.pointer = Some(point);
                        self.down = Some(point);
                        self.dragging = true;
                        self.frozen = self.latest.clone();

                        if let Some(buffer) = self.latest.clone() {
                            self.capture(&buffer);
                        }
                    }

                    return Ok(true);
                }

                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
                    if self.dragging =>
                {
                    let point = (mouse.column, mouse.row);
                    self.moved |= self.down != Some(point);
                    self.pointer = Some(point);
                    self.end = Some(self.position(point));

                    if matches!(mouse.kind, MouseEventKind::Up(_)) {
                        self.dragging = false;

                        if !self.moved {
                            self.clear();
                        }
                    }

                    return Ok(true);
                }

                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.pointer = Some((mouse.column, mouse.row));
                    self.begin_scroll();

                    // A drag belongs to its starting region, even outside its bounds.
                    if self.dragging && self.region != Some(Region::Transcript) {
                        return Ok(true);
                    }
                }

                _ => {}
            },

            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if self.active()
                    && key.code == KeyCode::Char('c')
                    && key
                        .modifiers
                        .contains(KeyModifiers::CONTROL)
                {
                    let text = self.text();

                    if !text.is_empty() {
                        copy_text(&text)?;
                    }

                    return Ok(true);
                }

                if self.active() && key.code == KeyCode::Esc {
                    self.clear();
                    return Ok(true);
                }

                self.clear();
            }

            Event::Resize(..) | Event::Paste(_) => self.clear(),
            _ => {}
        }

        Ok(false)
    }

    fn bounds(&self) -> Option<((u16, usize), (u16, usize))> {
        let (a, b) = (self.anchor?, self.end?);

        Some(if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        })
    }

    fn selected_columns(&self, row: usize) -> Option<(u16, u16)> {
        if !self.moved {
            return None;
        }

        let (start, end) = self.bounds()?;

        if row < start.1 || row > end.1 {
            return None;
        }

        Some((
            if row == start.1 { start.0 } else { 0 },
            if row == end.1 {
                end.0
            } else {
                self.area().width.saturating_sub(1)
            },
        ))
    }

    fn capture(&mut self, buffer: &Buffer) {
        if !self.active() {
            return;
        }

        let area = self.area().intersection(buffer.area);

        for y in area.y..area.bottom() {
            let row = (y - area.y) as usize + self.base();
            self.rows.entry(row).or_insert_with(|| {
                (area.x..area.right())
                    .map(|x| buffer[(x, y)].clone())
                    .collect()
            });
        }
    }

    /// Fill rows skipped by a wheel tick, including when the viewport is shorter
    /// than the scroll step. Copying must include the entire selected interval.
    pub(super) fn transcript_view(
        &mut self,
        offset: usize,
        layout: &super::transcript::TranscriptLayout,
    ) {
        self.offset = offset;
        if !self.active() {
            self.decorations.clear();
        }
        for row in offset
            ..offset
                .saturating_add(usize::from(self.viewport.height))
                .min(layout.height())
        {
            self.decorations
                .entry(row)
                .or_insert_with(|| {
                    layout
                        .selection_metadata(row)
                        .cloned()
                        .unwrap_or_default()
                });
        }

        if self.region != Some(Region::Transcript) {
            return;
        }

        if self.extend {
            if let Some(point) = self.pointer {
                self.end = Some(self.position(point));
            }

            self.extend = false;
        }

        let Some((start, end)) = self.bounds() else {
            return;
        };
        if self.filled == Some((start.1, end.1)) {
            return;
        }

        for row in start.1..=end.1 {
            self.decorations
                .entry(row)
                .or_insert_with(|| {
                    layout
                        .selection_metadata(row)
                        .cloned()
                        .unwrap_or_default()
                });
            self.rows.entry(row).or_insert_with(|| {
                let area = Rect::new(0, 0, self.viewport.width, 1);
                let mut buffer = Buffer::empty(area);
                layout.draw_buffer(&mut buffer, area, row);
                buffer.content
            });
        }

        self.filled = Some((start.1, end.1));
    }

    pub fn text(&self) -> String {
        let mut lines = Vec::new();
        let media = self.selected_media();
        // A clipped picture can be selected while its text baseline is offscreen.
        // Keep copy order tied to that baseline and emit its source exactly once.
        let mut rows = self
            .rows
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        rows.extend(media.iter().map(|m| m.baseline));

        for row in rows {
            let columns = self.selected_columns(row);
            let copies = media
                .iter()
                .filter(|m| m.baseline == row)
                .collect::<Vec<_>>();
            if columns.is_none() && copies.is_empty() {
                continue;
            }
            let metadata = self.row_decorations(row);
            let chrome = metadata.is_some_and(|metadata| metadata.chrome);
            if chrome && copies.is_empty() {
                continue;
            }
            let decorations = metadata.map_or(&[][..], |metadata| metadata.ranges.as_slice());
            let cells = self.rows.get(&row);
            let len = cells.map_or(usize::from(self.area().width), Vec::len);

            let mut text = String::new();
            let mut x = 0;

            while x < len {
                if let Some(copy) = copies
                    .iter()
                    .find(|m| m.block.columns.start == x)
                {
                    text.push_str(&copy.text);
                    x = copy.block.columns.end;
                    continue;
                }

                let Some(cell) = cells.and_then(|cells| cells.get(x)) else {
                    x += 1;
                    continue;
                };
                let width = cell.symbol().width().max(1);

                if !chrome
                    && columns.is_some_and(|(left, right)| {
                        x <= right as usize && x + width > left as usize
                    })
                {
                    if decorations
                        .iter()
                        .any(|range| range.start < x + width && range.end > x)
                    {
                        // Remove gutters, but keep interior columns separated (e.g. tables).
                        // Trailing padding is trimmed with the rest of the terminal row.
                        if !text.is_empty() {
                            text.extend(std::iter::repeat_n(' ', width));
                        }
                    } else {
                        text.push_str(cell.symbol());
                    }
                }

                x += width;
            }

            lines.push(text.trim_end().to_owned());
        }

        lines.join("\n")
    }

    fn selected_media(&self) -> Vec<SelectedMedia> {
        if self.region != Some(Region::Transcript) || !self.moved {
            return Vec::new();
        }
        let Some((start, end)) = self.bounds() else {
            return Vec::new();
        };
        let mut blocks = BTreeMap::new();
        for (&row, metadata) in self.decorations.range(start.1..=end.1) {
            let Some((left, right)) = self.selected_columns(row) else {
                continue;
            };
            for copy in &metadata.copy {
                if copy.columns.start > usize::from(right) || copy.columns.end <= usize::from(left)
                {
                    continue;
                }

                let top = row.saturating_sub(usize::from(copy.row));
                blocks
                    .entry((top, copy.columns.start))
                    .or_insert_with(|| SelectedMedia {
                        block: super::media::SelectionBlock {
                            rows: top..top + usize::from(copy.height),
                            columns: copy.columns.clone(),
                        },
                        baseline: top + usize::from(copy.baseline),
                        text: copy.text.clone(),
                    });
            }
        }

        blocks.into_values().collect()
    }

    fn row_decorations(&self, row: usize) -> Option<&super::transcript::Decorations> {
        (self.region == Some(Region::Transcript))
            .then(|| self.decorations.get(&row))
            .flatten()
    }

    pub fn draw_frozen(&mut self, frame: &mut Frame<'_>) -> bool {
        let Some(buffer) = &self.frozen else {
            return false;
        };
        if buffer.area != frame.area() {
            return false;
        }

        *frame.buffer_mut() = buffer.clone();
        super::media::frozen_frame();
        self.paint(frame);

        true
    }

    pub fn paint(&mut self, frame: &mut Frame<'_>) {
        let snapshot = frame.buffer_mut().clone();

        self.capture(&snapshot);

        if self.frozen.is_some() {
            self.frozen = Some(snapshot.clone());
        }

        self.latest = Some(snapshot);
        let area = self.area().intersection(frame.area());
        let media = self
            .selected_media()
            .into_iter()
            .map(|m| m.block)
            .collect::<Vec<_>>();

        super::media::select(area, self.base(), &media);
        for block in &media {
            for row in block.rows.start.max(self.base())
                ..block
                    .rows
                    .end
                    .min(self.base() + usize::from(area.height))
            {
                let y = area.y + (row - self.base()) as u16;
                for x in block
                    .columns
                    .clone()
                    .take_while(|&x| x < usize::from(area.width))
                {
                    frame.buffer_mut()[(area.x + x as u16, y)]
                        .set_bg(super::theme::THEME.selection);
                }
            }
        }

        let mut styles = HashMap::new();

        for y in area.y..area.bottom() {
            let row = (y - area.y) as usize + self.base();
            let Some((left, right)) = self.selected_columns(row) else {
                continue;
            };
            let Some(cells) = self.rows.get(&row) else {
                continue;
            };

            let decorations = self
                .row_decorations(row)
                .map_or(&[][..], |metadata| metadata.ranges.as_slice());
            let is_content = |index: usize| {
                !cells[index].symbol().trim().is_empty()
                    && !decorations
                        .iter()
                        .any(|range| range.contains(&index))
            };
            let Some(start) = (0..cells.len()).find(|&index| is_content(index)) else {
                continue;
            };
            let last = (start..cells.len())
                .rfind(|&index| is_content(index))
                .unwrap();
            let end = last + cells[last].symbol().width().max(1);

            let mut x = 0;

            while x < cells.len().min(area.width as usize) {
                let width = cells[x].symbol().width().max(1);

                if x >= start
                    && x < end
                    && x <= right as usize
                    && x + width > left as usize
                    && !decorations
                        .iter()
                        .any(|range| range.start < x + width && range.end > x)
                {
                    let style = *styles
                        .entry(cells[x].style())
                        .or_insert_with_key(|style| super::theme::selection_style(*style));
                    for (column, cell) in cells
                        .iter()
                        .enumerate()
                        .take((x + width).min(area.width as usize))
                        .skip(x)
                    {
                        frame.buffer_mut()[(area.x + column as u16, y)] = cell.clone();
                        frame.buffer_mut()[(area.x + column as u16, y)].set_style(style);
                    }
                }

                x += width;
            }
        }
    }
}

#[cfg(test)]
mod tests;
