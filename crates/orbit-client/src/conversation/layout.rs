//! Chronological conversation segments shared by both frontends.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Text,
    Thought,
    Tool,
    Notice,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Layout {
    pub order: Vec<usize>,
    pub segments: Vec<Segment>,
}

/// A source item or an uninterrupted run of tool calls, in arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Part(usize),
    Tools(Range<usize>),
}

/// Keeps prose and reasoning in place. Only adjacent tools share a disclosure.
/// Updating a background result does not create or move a segment.
pub fn partition(roles: &[Role]) -> Layout {
    let mut layout = Layout {
        order: (0..roles.len()).collect(),
        segments: Vec::new(),
    };
    for (index, role) in roles.iter().enumerate() {
        if *role == Role::Tool {
            if let Some(Segment::Tools(range)) = layout.segments.last_mut() {
                range.end = index + 1;
            } else {
                layout
                    .segments
                    .push(Segment::Tools(index..index + 1));
            }
        } else {
            layout
                .segments
                .push(Segment::Part(index));
        }
    }
    layout
}

/// Finds adjacent calls around an existing tool. Non-tool indices are single items.
pub fn tool_range(len: usize, index: usize, is_tool: impl Fn(usize) -> bool) -> Range<usize> {
    if index >= len {
        return len..len;
    }
    let mut range = index..index + 1;
    if is_tool(index) {
        while range.start > 0 && is_tool(range.start - 1) {
            range.start -= 1;
        }
        while range.end < len && is_tool(range.end) {
            range.end += 1;
        }
    }
    range
}

/// Prose before the last tool is progress; all prose after it belongs to the answer.
pub fn answer_start(roles: impl DoubleEndedIterator<Item = (usize, Role)>) -> usize {
    roles
        .rev()
        .find(|(_, role)| *role == Role::Tool)
        .map_or(0, |(index, _)| index + 1)
}

/// Finds the last prose segment after tool execution, ignoring reasoning and notices.
pub fn conclusion(roles: impl DoubleEndedIterator<Item = (usize, Role)>) -> Option<usize> {
    roles
        .rev()
        .find(|(_, role)| matches!(role, Role::Text | Role::Tool))
        .filter(|(_, role)| *role == Role::Text)
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::{Role::*, *};

    #[test]
    fn interleaved_requests_preserve_prose_and_group_only_adjacent_calls() {
        let roles = [User, Thought, Text, Tool, Tool, Thought, Tool, Text, Notice];
        let layout = partition(&roles);
        assert_eq!(layout.order, (0..roles.len()).collect::<Vec<_>>());
        assert_eq!(
            layout.segments,
            [
                Segment::Part(0),
                Segment::Part(1),
                Segment::Part(2),
                Segment::Tools(3..5),
                Segment::Part(5),
                Segment::Tools(6..7),
                Segment::Part(7),
                Segment::Part(8),
            ]
        );
        assert_eq!(tool_range(roles.len(), 4, |i| roles[i] == Tool), 3..5);
        assert_eq!(tool_range(roles.len(), 6, |i| roles[i] == Tool), 6..7);
    }

    #[test]
    fn later_input_never_takes_an_earlier_answer_or_reasoning() {
        let layout = partition(&[User, Thought, Text, User, Tool, Text, Notice]);
        assert_eq!(layout.order, [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(layout.segments.len(), 7);
    }
}
