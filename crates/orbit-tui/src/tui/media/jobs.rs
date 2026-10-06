use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use super::{Key, Prepared};

#[derive(Default, Debug)]
pub(super) struct PreviewJobs {
    closed: AtomicBool,
    generation: AtomicU64,
    requested: Mutex<Option<Key>>,
    ready: Mutex<Option<Arc<Prepared>>>,
}

#[derive(Clone)]
pub(super) struct Ticket {
    owner: Arc<PreviewJobs>,
    generation: u64,
}

pub(super) struct Work {
    pub key: Key,
    pub ticket: Option<Ticket>,
}

impl PreviewJobs {
    pub fn request(self: &Arc<Self>, key: &Key) -> Ticket {
        let mut requested = self.requested.lock().unwrap();
        if requested.as_ref() != Some(key) {
            self.generation.fetch_add(1, Ordering::Release);
            *requested = Some(key.clone());
        }
        Ticket {
            owner: self.clone(),
            generation: self.generation.load(Ordering::Acquire),
        }
    }

    pub fn take_ready(&self) -> Option<Arc<Prepared>> {
        self.ready.lock().ok()?.take()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

impl Ticket {
    pub fn same_request(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner) && self.generation == other.generation
    }

    pub fn current(&self) -> bool {
        !self.owner.closed.load(Ordering::Acquire)
            && self.generation == self.owner.generation.load(Ordering::Acquire)
    }

    pub fn publish(&self, picture: Arc<Prepared>) {
        // An in-flight frame is still useful while a newer view is rendering.
        if !self.owner.closed.load(Ordering::Acquire)
            && let Ok(mut ready) = self.owner.ready.lock()
        {
            *ready = Some(picture);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::media::{Cached, Context, Source, Value, View, decode::Decoder};
    use ratatui_image::picker::Picker;
    use std::time::{Duration, Instant};

    #[test]
    fn newer_views_cancel_queued_work_but_keep_in_flight_frames_until_close() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture.png");
        image::RgbaImage::new(8, 8).save(&path).unwrap();
        let context = Context::new(Picker::halfblocks());
        let mut key = context.key(&Source::Image(path.to_string_lossy().into()), 8, 8, true);
        key.view = Some(View::default());
        let picture = Arc::new(
            Decoder::default()
                .prepare(&key, &Picker::halfblocks())
                .unwrap(),
        );
        let jobs = Arc::new(PreviewJobs::default());
        let first = jobs.request(&key);
        let repeated = jobs.request(&key);
        assert!(first.current() && repeated.current());
        assert!(first.same_request(&repeated));
        assert!(!first.same_request(&Arc::new(PreviewJobs::default()).request(&key)));

        key.view.as_mut().unwrap().zoom = 200;
        let latest = jobs.request(&key);
        assert!(!first.current() && !repeated.current());
        assert!(latest.current());
        first.publish(picture.clone());
        assert!(Arc::ptr_eq(&picture, &jobs.take_ready().unwrap()));
        assert!(jobs.take_ready().is_none());

        key.view.as_mut().unwrap().zoom = 100;
        let returning = jobs.request(&key);
        let shared = context.shared.as_ref().unwrap();
        shared.cache.lock().unwrap().insert(
            key.clone(),
            Cached {
                value: Value::Pending(Some(returning.clone())),
                touched: 0,
            },
        );
        let revision = shared.revision.load(Ordering::Acquire);
        shared.requests[0]
            .send(Work {
                key: key.clone(),
                ticket: Some(first),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.revision.load(Ordering::Acquire) == revision {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            &shared.cache.lock().unwrap().get(&key).unwrap().value,
            Value::Pending(Some(pending)) if returning.same_request(pending)
        ));
        shared.requests[0]
            .send(Work {
                key: key.clone(),
                ticket: Some(returning.clone()),
            })
            .unwrap();
        loop {
            if jobs.take_ready().is_some() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            &shared.cache.lock().unwrap().get(&key).unwrap().value,
            Value::Ready(_)
        ));

        jobs.close();
        assert!(!returning.current());
        returning.publish(picture);
        assert!(jobs.take_ready().is_none());
    }
}
