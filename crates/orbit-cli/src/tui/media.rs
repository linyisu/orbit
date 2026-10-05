//! Shared, asynchronous picture/TeX pipeline. Layout and copy retain source text;
//! protocol bytes are painted only after text selection has captured the frame.
mod decode;
mod flow;
mod preview;
#[cfg(test)]
mod tests;
mod view;

use std::{
    cell::RefCell,
    collections::HashMap,
    ops::Range,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
};

use ratatui::{
    buffer::Buffer,
    layout::{Rect, Size},
    widgets::Widget,
};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    sliced::{SlicedImage, SlicedProtocol},
};

pub(super) use flow::{layout, layout_in_cell, requests, requests_in_cell};
pub(super) use preview::Preview;
use view::View;

const MAX_CACHE_ENTRIES: usize = 48;
const WORKER_QUEUE_CAPACITY: usize = 32;
const MAX_CACHE_BYTES: usize = 96 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Source {
    Image(String),
    Math { tex: String, display: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub range: Range<usize>,
    pub source: Source,
}

#[derive(Clone, Debug)]
pub(super) struct Request {
    pub source: Source,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug)]
pub(super) struct CopyText {
    pub columns: Range<usize>,
    pub row: u16,
    pub height: u16,
    pub baseline: u16,
    pub text: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectionBlock {
    pub rows: Range<usize>,
    pub columns: Range<usize>,
}

pub(super) struct Prepared {
    protocol: SlicedProtocol,
    selected_protocol: Option<SlicedProtocol>,
    pub source: Source,
    pub size: Size,
    pub baseline: u16,
    pan_extent: (f32, f32),
    kitty_id: Option<u32>,
    selected_kitty_id: Option<u32>,
}

static DELETIONS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

impl Drop for Prepared {
    fn drop(&mut self) {
        if let Ok(mut deletions) = DELETIONS.lock() {
            deletions.extend(
                [self.kitty_id, self.selected_kitty_id]
                    .into_iter()
                    .flatten(),
            );
        }
    }
}

fn image_id() -> u32 {
    use std::hash::BuildHasher;
    static IDS: OnceLock<AtomicU32> = OnceLock::new();
    IDS.get_or_init(|| {
        AtomicU32::new(
            std::collections::hash_map::RandomState::new().hash_one("orbit-media") as u32,
        )
    })
    .fetch_add(1, Ordering::Relaxed)
    .max(1)
}

pub(super) fn delete_stale(writer: &mut impl std::io::Write) -> std::io::Result<()> {
    if let Ok(mut deletions) = DELETIONS.lock() {
        for id in deletions.drain(..) {
            write!(writer, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
        }
    }

    Ok(())
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(super) struct Fragment {
    pub picture: Arc<Prepared>,
    pub x: u16,
    pub row: u16,
}

/// Policy identity belongs in both decoded and raw-byte cache keys: a resumed
/// session may have different proxy settings while sharing terminal workers.
#[derive(Clone, Debug, Default)]
struct Network(Arc<orbit_tools::web_fetch::WebClient>);

impl PartialEq for Network {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Network {}

impl std::hash::Hash for Network {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&Arc::as_ptr(&self.0), state);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    network: Option<Network>,
    source: Source,
    workspace: PathBuf,
    width: u16,
    height: u16,
    large: bool,
    view: Option<View>,
}

enum Value {
    Pending,
    Ready(Arc<Prepared>),
    Failed(String),
}

impl Value {
    fn bytes(&self, picker: &Picker) -> usize {
        match self {
            Self::Ready(p) => {
                usize::from(p.size.width)
                    * usize::from(p.size.height)
                    * usize::from(picker.font_size().width)
                    * usize::from(picker.font_size().height)
                    * if p.selected_protocol.is_some() { 16 } else { 8 }
            }

            _ => 0,
        }
    }
}

struct Cached {
    value: Value,
    touched: u64,
}

struct Shared {
    picker: Picker,
    cache: Mutex<HashMap<Key, Cached>>,
    requests: [mpsc::SyncSender<Key>; 2],
    revision: AtomicU64,
    clock: AtomicU64,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaCache")
            .field("picker", &self.picker)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Context {
    network: Network,
    shared: Option<Arc<Shared>>,
    pub workspace: PathBuf,
    seen: u64,
}

impl Context {
    /// Called before EventStream exists; capability replies must have one reader.
    pub fn detect(client: Arc<orbit_tools::web_fetch::WebClient>) -> Self {
        let mut context = Self::new(
            Picker::from_query_stdio_with_options(
                ratatui_image::picker::cap_parser::QueryStdioOptions {
                    terminal_background_color_osc: true,
                    ..Default::default()
                },
            )
            .unwrap_or_else(|_| Picker::halfblocks()),
        );

        context.set_client(client);
        context
    }

    pub fn set_client(&mut self, client: Arc<orbit_tools::web_fetch::WebClient>) {
        if Arc::ptr_eq(&self.network.0, &client) {
            return;
        }
        self.network = Network(client);
        if let Some(shared) = &self.shared {
            shared.revision.fetch_add(1, Ordering::Release);
        }
    }

    fn new(picker: Picker) -> Self {
        let (local_send, local_receive) = mpsc::sync_channel::<Key>(WORKER_QUEUE_CAPACITY);
        let (remote_send, remote_receive) = mpsc::sync_channel::<Key>(WORKER_QUEUE_CAPACITY);
        let shared = Arc::new(Shared {
            picker,
            cache: Mutex::new(HashMap::new()),
            requests: [local_send, remote_send],
            revision: AtomicU64::new(0),
            clock: AtomicU64::new(0),
        });
        // Keep downloads off the local/formula queue. A slow image host must
        // not delay attachment thumbnails or TeX; both queues remain bounded.
        for (name, receive) in [
            ("orbit-media", local_receive),
            ("orbit-media-remote", remote_receive),
        ] {
            let weak = Arc::downgrade(&shared);
            // Weak ownership lets the channels close when the last UI/viewer leaves.
            let worker = std::thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    let mut decoder = decode::Decoder::default();
                    while let Ok(key) = receive.recv() {
                        let Some(shared) = weak.upgrade() else { break };
                        let result = decoder.prepare(&key, &shared.picker);
                        if let Ok(mut cache) = shared.cache.lock() {
                            let value = match result {
                                Ok(picture) => Value::Ready(Arc::new(picture)),
                                Err(error) => Value::Failed(error.to_string()),
                            };

                            while cache
                                .values()
                                .map(|c| c.value.bytes(&shared.picker))
                                .sum::<usize>()
                                + value.bytes(&shared.picker)
                                > MAX_CACHE_BYTES
                            {
                                let oldest = cache
                                    .iter()
                                    .filter(|(k, v)| {
                                        **k != key && matches!(v.value, Value::Ready(_))
                                    })
                                    .min_by_key(|(_, v)| v.touched)
                                    .map(|(k, _)| k.clone());

                                if let Some(oldest) = oldest {
                                    cache.remove(&oldest);
                                } else {
                                    break;
                                }
                            }

                            if let Some(entry) = cache.get_mut(&key) {
                                entry.value = value;
                            }
                            shared.revision.fetch_add(1, Ordering::Release);
                        }
                    }
                });
            if worker.is_err() {
                return Self::default();
            }
        }

        Self {
            shared: Some(shared),
            ..Self::default()
        }
    }

    pub fn enabled(&self) -> bool {
        self.shared.is_some()
    }

    pub fn revision(&self) -> u64 {
        self.shared
            .as_ref()
            .map_or(0, |s| s.revision.load(Ordering::Acquire))
    }

    pub fn changed(&mut self) -> bool {
        let next = self.revision();
        let changed = self.seen != next;
        self.seen = next;
        changed
    }

    pub fn invalidate(&self) {
        if let Some(shared) = &self.shared
            && let Ok(mut cache) = shared.cache.lock()
        {
            cache.clear();
            shared.revision.fetch_add(1, Ordering::Release);
        }
    }

    pub fn same_cache(&self, other: &Self) -> bool {
        (match (&self.shared, &other.shared) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }) && self.workspace == other.workspace
            && self.network == other.network
    }

    pub fn picture(
        &self,
        source: &Source,
        width: u16,
        height: u16,
        large: bool,
    ) -> Option<Arc<Prepared>> {
        self.lookup(self.key(source, width, height, large), true)
    }

    pub fn cached_picture(
        &self,
        source: &Source,
        width: u16,
        height: u16,
    ) -> Option<Arc<Prepared>> {
        self.lookup(self.key(source, width, height, false), false)
    }

    fn preview_picture(
        &self,
        source: &Source,
        width: u16,
        height: u16,
        view: View,
    ) -> Option<Arc<Prepared>> {
        let mut key = self.key(source, width, height, true);
        key.view = Some(view);
        self.lookup(key, true)
    }

    fn lookup(&self, key: Key, request: bool) -> Option<Arc<Prepared>> {
        let shared = self.shared.as_ref()?;
        // Pixel math is illegible in halfblocks. Keep selectable TeX in that case.
        if matches!(key.source, Source::Math { .. })
            && shared.picker.protocol_type() == ProtocolType::Halfblocks
        {
            return None;
        }

        if key.width == 0 || key.height == 0 {
            return None;
        }
        let mut cache = shared.cache.lock().ok()?;
        let touched = shared.clock.fetch_add(1, Ordering::Relaxed);
        if let Some(entry) = cache.get_mut(&key) {
            entry.touched = touched;
            return match &entry.value {
                Value::Ready(value) => Some(value.clone()),
                _ => None,
            };
        }

        if !request {
            return None;
        }
        if cache.len() >= MAX_CACHE_ENTRIES {
            let oldest = cache
                .iter()
                .filter(|(_, v)| !matches!(v.value, Value::Pending))
                .min_by_key(|(_, v)| v.touched)
                .map(|(k, _)| k.clone());

            cache.remove(&oldest?);
        }

        let remote = matches!(&key.source, Source::Image(url) if is_remote(url));
        if shared.requests[usize::from(remote)]
            .try_send(key.clone())
            .is_ok()
        {
            cache.insert(
                key,
                Cached {
                    value: Value::Pending,
                    touched,
                },
            );
        }

        None
    }

    #[cfg(test)]
    pub fn error(&self, source: &Source, width: u16, height: u16, large: bool) -> Option<String> {
        self.failure(self.key(source, width, height, large))
    }

    fn preview_error(
        &self,
        source: &Source,
        width: u16,
        height: u16,
        view: View,
    ) -> Option<String> {
        let mut key = self.key(source, width, height, true);
        key.view = Some(view);
        self.failure(key)
    }

    fn failure(&self, key: Key) -> Option<String> {
        let shared = self.shared.as_ref()?;
        if matches!(key.source, Source::Math { .. })
            && shared.picker.protocol_type() == ProtocolType::Halfblocks
        {
            return Some("当前终端未提供像素图形，正文保留公式源码".into());
        }

        let cache = shared.cache.lock().ok()?;
        match &cache.get(&key)?.value {
            Value::Failed(error) => Some(error.clone()),
            _ => None,
        }
    }

    fn key(&self, source: &Source, width: u16, height: u16, large: bool) -> Key {
        Key {
            network: matches!(source, Source::Image(url) if is_remote(url))
                .then(|| self.network.clone()),
            source: source.clone(),
            workspace: self.workspace.clone(),
            width,
            height,
            large,
            view: None,
        }
    }
}

pub(super) fn is_remote(source: &str) -> bool {
    url::Url::parse(source).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

#[derive(Clone)]
struct Placement {
    fragment: Fragment,
    area: Rect,
    selected: bool,
}

#[derive(Default)]
struct Layer {
    current: Vec<Placement>,
    previous: Vec<Placement>,
}

thread_local! { static LAYER: RefCell<Layer> = RefCell::new(Layer::default()); }

pub(super) fn begin_frame() {
    LAYER.with(|l| l.borrow_mut().current.clear());
}

pub(super) fn schedule(fragment: &Fragment, area: Rect) {
    if !area.is_empty() {
        LAYER.with(|l| {
            let mut layer = l.borrow_mut();
            // A hover refresh can redraw the same surface within one frame.
            // Painting a Kitty slice twice would overwrite its first upload.
            layer.current.retain(|p| p.area != area);
            layer.current.push(Placement {
                fragment: fragment.clone(),
                area,
                selected: false,
            })
        });
    }
}

pub(super) fn occlude(area: Rect) {
    LAYER.with(|l| {
        l.borrow_mut()
            .current
            .retain(|p| p.area.intersection(area).is_empty())
    });
}

pub(super) fn frozen_frame() {
    LAYER.with(|l| {
        let mut l = l.borrow_mut();
        l.current = l.previous.clone();
    });
}

/// Match complete picture occurrences, rather than every use of a cached image.
/// Offscreen slices share the same absolute origin with their visible slices.
pub(super) fn select(area: Rect, offset: usize, blocks: &[SelectionBlock]) {
    LAYER.with(|layer| {
        for p in &mut layer.borrow_mut().current {
            if !area.contains((p.area.x, p.area.y).into()) {
                continue;
            }
            p.selected = blocks.iter().any(|block| {
                let row = offset + usize::from(p.area.y - area.y);
                row.checked_sub(usize::from(p.fragment.row)) == Some(block.rows.start)
                    && usize::from(p.area.x - area.x) == block.columns.start
                    && usize::from(p.fragment.picture.size.width) == block.columns.len()
            });
        }
    });
}

pub(super) fn paint(buffer: &mut Buffer) {
    LAYER.with(|l| {
        let mut layer = l.borrow_mut();
        for p in &layer.current {
            let area = p.area.intersection(buffer.area);
            if !area.is_empty() {
                SlicedImage::new(
                    if p.selected {
                        p.fragment
                            .picture
                            .selected_protocol
                            .as_ref()
                            .unwrap_or(&p.fragment.picture.protocol)
                    } else {
                        &p.fragment.picture.protocol
                    },
                    (0, -(p.fragment.row as i16)).into(),
                )
                .render(area, buffer);
            }
        }

        layer.previous = layer.current.clone();
    });
}

pub(super) fn clear_layers() {
    LAYER.with(|l| *l.borrow_mut() = Layer::default());
}
