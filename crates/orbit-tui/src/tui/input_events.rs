use std::{io, task::Poll};

use crossterm::event::{Event, MouseEventKind};
use futures_util::{Stream, StreamExt, future::poll_fn};

const MAX_MOTION_BATCH: usize = 256;

/// Keep the newest consecutive pointer position without crossing input boundaries.
pub(super) async fn next<S>(
    stream: &mut S,
    pending: &mut Option<io::Result<Event>>,
) -> Option<io::Result<Event>>
where
    S: Stream<Item = io::Result<Event>> + Unpin,
{
    let mut event = match pending.take() {
        Some(event) => event,
        None => stream.next().await?,
    };
    let Ok(Event::Mouse(mouse)) = &event else {
        return Some(event);
    };
    if !matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
        return Some(event);
    }
    let kind = mouse.kind;
    let modifiers = mouse.modifiers;
    for _ in 1..MAX_MOTION_BATCH {
        let available = poll_fn(|cx| Poll::Ready(stream.poll_next_unpin(cx))).await;
        match available {
            Poll::Ready(Some(Ok(Event::Mouse(mouse))))
                if mouse.kind == kind && mouse.modifiers == modifiers =>
            {
                event = Ok(Event::Mouse(mouse));
            }
            Poll::Ready(Some(next)) => {
                *pending = Some(next);
                break;
            }
            Poll::Ready(None) | Poll::Pending => break,
        }
    }
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent};
    use futures_util::stream;

    fn mouse(kind: MouseEventKind, column: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row: 8,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[tokio::test]
    async fn pointer_bursts_keep_the_latest_position_and_preserve_release_and_keyboard_order() {
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 0);
        let release = mouse(MouseEventKind::Up(MouseButton::Left), 100);
        let key = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut input = vec![Ok(down.clone())];
        input.extend((1..=100).map(|x| Ok(mouse(MouseEventKind::Drag(MouseButton::Left), x))));
        input.extend([Ok(release.clone()), Ok(key.clone())]);
        let mut stream = stream::iter(input);
        let mut pending = None;
        for expected in [
            down,
            mouse(MouseEventKind::Drag(MouseButton::Left), 100),
            release,
            key,
        ] {
            assert_eq!(
                next(&mut stream, &mut pending)
                    .await
                    .unwrap()
                    .unwrap(),
                expected
            );
        }
        assert!(
            next(&mut stream, &mut pending)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn motion_does_not_wait_for_another_event_or_discard_errors_and_direction_changes() {
        let left = mouse(MouseEventKind::Drag(MouseButton::Left), 2);
        let right = mouse(MouseEventKind::Drag(MouseButton::Right), 3);
        let mut stream = stream::iter([
            Ok(left.clone()),
            Ok(right.clone()),
            Err(io::Error::other("input failed")),
        ])
        .chain(stream::pending());
        let mut pending = None;
        assert_eq!(
            next(&mut stream, &mut pending)
                .await
                .unwrap()
                .unwrap(),
            left
        );
        assert_eq!(
            next(&mut stream, &mut pending)
                .await
                .unwrap()
                .unwrap(),
            right
        );
        assert_eq!(
            next(&mut stream, &mut pending)
                .await
                .unwrap()
                .unwrap_err()
                .to_string(),
            "input failed"
        );
        let mut stream = stream::iter([Ok(left.clone())]).chain(stream::pending());
        assert_eq!(
            next(&mut stream, &mut pending)
                .await
                .unwrap()
                .unwrap(),
            left
        );
    }
}
