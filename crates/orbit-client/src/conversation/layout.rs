//! Stable source indices for the three conversation regions in either frontend.

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
    pub progress: Vec<usize>,
}

/// Groups reasoning and calls without changing their internal order or discarding progress.
/// Only user input starts a new region; model requests and background returns do not.
pub fn partition(roles: &[Role]) -> Layout {
    let mut layout = Layout::default();
    let mut start = 0;
    for end in 1..=roles.len() {
        if end == roles.len() || roles[end] == Role::User {
            append(&mut layout, roles, start..end);
            start = end;
        }
    }
    layout
}

fn append(layout: &mut Layout, roles: &[Role], range: std::ops::Range<usize>) {
    let start = answer_start(
        range
            .clone()
            .map(|index| (index, roles[index])),
    );
    let is_answer = |index: &usize| roles[*index] == Role::Text && *index >= start;
    for role in [Role::User, Role::Thought, Role::Tool] {
        layout.order.extend(
            range
                .clone()
                .filter(|index| roles[*index] == role),
        );
    }
    layout
        .order
        .extend(range.clone().filter(is_answer));
    layout.order.extend(
        range
            .clone()
            .filter(|index| roles[*index] == Role::Notice),
    );
    layout
        .progress
        .extend(range.filter(|index| roles[*index] == Role::Text && !is_answer(index)));
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
    fn interleaved_requests_keep_reasoning_calls_and_answer_separate() {
        let layout = partition(&[User, Text, Tool, Thought, Text, Tool, Thought, Text]);
        assert_eq!(layout.order, [0, 3, 6, 2, 5, 7]);
        assert_eq!(layout.progress, [1, 4]);
    }

    #[test]
    fn later_input_never_takes_an_earlier_answer_or_reasoning() {
        let layout = partition(&[User, Thought, Text, User, Tool, Text, Notice]);
        assert_eq!(layout.order, [0, 1, 2, 3, 4, 5, 6]);
        assert!(layout.progress.is_empty());
    }
}
