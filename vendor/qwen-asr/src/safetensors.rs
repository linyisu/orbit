//! Safetensors mmap reader with multi-shard support.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_TENSORS: usize = 1024;

/// Name prefix of the audio encoder's tensors. The encoder prepacks all of its
/// large BF16 weights out of the mmap via `pread` at load and never faults them
/// through the mapping, so these ranges are excluded from the WILLNEED hint and
/// the parallel prefault to keep their pages out of the resident set (R5-J).
const ENCODER_TENSOR_PREFIX: &str = "thinker.audio_tower.";

fn is_pread_loaded(t: &TensorMeta) -> bool {
    t.dtype == Dtype::BF16 && t.name.starts_with(ENCODER_TENSOR_PREFIX)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dtype {
    F32,
    F16,
    BF16,
    I32,
    I64,
    Bool,
    Unknown,
}

impl Dtype {
    fn from_str(s: &str) -> Self {
        match s {
            "F32" => Dtype::F32,
            "F16" => Dtype::F16,
            "BF16" => Dtype::BF16,
            "I32" => Dtype::I32,
            "I64" => Dtype::I64,
            "BOOL" => Dtype::Bool,
            _ => Dtype::Unknown,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TensorMeta {
    pub name: String,
    pub dtype: Dtype,
    pub shape: Vec<i64>,
    pub data_offset: usize,
    pub data_size: usize,
}

impl TensorMeta {
    pub fn numel(&self) -> usize {
        self.shape.iter().product::<i64>() as usize
    }
}

pub struct SafetensorsFile {
    file: File,
    _mapping: memmap2::Mmap,
    data: *mut u8,
    header_size: usize,
    pub tensors: Vec<TensorMeta>,
    tensor_map: HashMap<String, usize>,
}

unsafe impl Send for SafetensorsFile {}
unsafe impl Sync for SafetensorsFile {}

impl SafetensorsFile {
    pub fn open(path: &str) -> Option<Self> {
        let file = File::open(path).ok()?;
        let file_size = usize::try_from(file.metadata().ok()?.len()).ok()?;
        if file_size < 8 {
            return None;
        }
        // SAFETY: verified model assets remain immutable while the model owns this mapping.
        let mapping = unsafe { memmap2::MmapOptions::new().map(&file).ok()? };
        let data = mapping.as_ptr() as *mut u8;

        // Read header size (first 8 bytes, little-endian u64). Parse the header
        // BEFORE prefaulting so encoder tensor byte ranges are known and can be
        // excluded from the WILLNEED hint / prefault.
        let header_size = unsafe {
            let mut buf = [0u8; 8];
            std::ptr::copy_nonoverlapping(data, buf.as_mut_ptr(), 8);
            u64::from_le_bytes(buf) as usize
        };

        if header_size > file_size - 8 {
            return None;
        }

        let header_json = std::str::from_utf8(&mapping[8..8 + header_size]).ok()?;
        let tensors = parse_header(header_json)?;
        for tensor in &tensors {
            let end = tensor.data_offset.checked_add(tensor.data_size)?;
            if end > file_size - 8 - header_size {
                return None;
            }
        }
        let mut tensor_map = HashMap::new();
        for (i, t) in tensors.iter().enumerate() {
            tensor_map.insert(t.name.clone(), i);
        }

        // Build a per-page skip bitmap covering the *interior* pages of every
        // pread-loaded (encoder BF16) tensor, then prefault only the pages that
        // are not fully inside such a range. Boundary pages shared with adjacent
        // decoder tensors are still prefaulted. Decoder residency is unchanged.
        let data_base = 8 + header_size;
        let skip = build_prefault_skip(&tensors, data_base, file_size);
        prefault_with_skip(data as *const u8, file_size, &skip);

        Some(SafetensorsFile {
            file,
            _mapping: mapping,
            data,
            header_size,
            tensors,
            tensor_map,
        })
    }

    /// Read `dst.len()` bytes of `tensor`'s data starting `byte_off` bytes into
    /// the tensor, via `pread(2)`. Reads through the file descriptor rather than
    /// the mmap, so the pages never enter this task's resident set. `pread` takes
    /// an explicit offset and does not touch the shared fd offset, so concurrent
    /// calls from multiple loader threads are safe.
    pub fn read_tensor_bytes(&self, tensor: &TensorMeta, byte_off: usize, dst: &mut [u8]) -> bool {
        let base = 8 + self.header_size + tensor.data_offset + byte_off;
        let mut done = 0usize;
        while done < dst.len() {
            #[cfg(unix)]
            let result = {
                use std::os::unix::fs::FileExt;
                self.file.read_at(&mut dst[done..], (base + done) as u64)
            };
            #[cfg(windows)]
            let result = {
                use std::os::windows::fs::FileExt;
                self.file.seek_read(&mut dst[done..], (base + done) as u64)
            };
            match result {
                Ok(0) => return false,
                Ok(count) => done += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        true
    }

    pub fn find(&self, name: &str) -> Option<&TensorMeta> {
        self.tensor_map.get(name).map(|&i| &self.tensors[i])
    }

    pub fn data_ptr(&self, tensor: &TensorMeta) -> *const u8 {
        unsafe { self.data.add(8 + self.header_size + tensor.data_offset) }
    }

    /// Get tensor data as f32 Vec (converts from BF16 if needed).
    pub fn get_f32(&self, tensor: &TensorMeta) -> Option<Vec<f32>> {
        let n = tensor.numel();
        if n == 0 {
            return None;
        }

        let ptr = self.data_ptr(tensor);
        match tensor.dtype {
            Dtype::F32 => {
                let mut out = vec![0.0f32; n];
                unsafe {
                    std::ptr::copy_nonoverlapping(ptr as *const f32, out.as_mut_ptr(), n);
                }
                Some(out)
            }
            Dtype::BF16 => {
                let src = unsafe { std::slice::from_raw_parts(ptr as *const u16, n) };
                let mut out = vec![0.0f32; n];
                for i in 0..n {
                    out[i] = f32::from_bits((src[i] as u32) << 16);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Get direct pointer to BF16 data in mmap.
    pub fn get_bf16_direct(&self, tensor: &TensorMeta) -> Option<*const u16> {
        if tensor.dtype != Dtype::BF16 {
            return None;
        }
        Some(self.data_ptr(tensor) as *const u16)
    }
}

#[cfg(unix)]
fn page_size() -> usize {
    let ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if ps > 0 {
        ps as usize
    } else {
        16 * 1024
    }
}

#[cfg(windows)]
fn page_size() -> usize {
    // All supported Windows native targets use 4 KiB mapping pages.
    4 * 1024
}

/// Mark the *interior* pages of every pread-loaded tensor as skip=true, so they
/// are never faulted resident. A page is skipped only if it lies entirely inside
/// such a tensor's byte range (start rounded up, end rounded down); boundary
/// pages shared with neighbouring decoder tensors stay prefaulted.
fn build_prefault_skip(tensors: &[TensorMeta], data_base: usize, file_size: usize) -> Vec<bool> {
    let ps = page_size();
    let n_pages = file_size.div_ceil(ps);
    let mut skip = vec![false; n_pages];
    for t in tensors {
        if !is_pread_loaded(t) {
            continue;
        }
        let start_byte = data_base + t.data_offset;
        let end_byte = start_byte + t.data_size;
        let first_full = start_byte.div_ceil(ps);
        let last_full = end_byte / ps; // exclusive
        let mut p = first_full;
        while p < last_full && p < n_pages {
            skip[p] = true;
            p += 1;
        }
    }
    skip
}

/// Prefault every page not marked in `skip`, in parallel. Non-skipped pages also
/// get an `MADV_WILLNEED` hint (coalesced into contiguous runs) so the readahead
/// stays scoped to decoder ranges. Skipped (encoder BF16) pages are neither
/// hinted nor touched, keeping them out of the resident set.
fn prefault_with_skip(data: *const u8, file_size: usize, skip: &[bool]) {
    let ps = page_size();
    let n_pages = file_size.div_ceil(ps);
    if n_pages == 0 {
        return;
    }

    // WILLNEED on contiguous non-skipped runs only.
    #[cfg(unix)]
    {
        let base_addr = data as usize;
        let mut run_start: Option<usize> = None;
        for (page, &skip_page) in skip.iter().enumerate() {
            match (run_start, !skip_page) {
                (None, true) => run_start = Some(page),
                (Some(s), false) => {
                    let off = s * ps;
                    let end = (page * ps).min(file_size);
                    unsafe {
                        libc::madvise((base_addr + off) as *mut _, end - off, libc::MADV_WILLNEED);
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        // Close a run that extends to the end of the file.
        if let Some(s) = run_start {
            let off = s * ps;
            unsafe {
                libc::madvise(
                    (base_addr + off) as *mut _,
                    file_size - off,
                    libc::MADV_WILLNEED,
                );
            }
        }

    }

    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(n_pages);
    let checksum = AtomicUsize::new(0);
    let base = data as usize;

    std::thread::scope(|scope| {
        for tid in 0..n_threads {
            let checksum = &checksum;
            scope.spawn(move || {
                let mut local = 0usize;
                let mut page = tid;
                while page < n_pages {
                    if !skip[page] {
                        let off = page * ps;
                        if off < file_size {
                            let ptr = (base + off) as *const u8;
                            local ^= unsafe { std::ptr::read_volatile(ptr) } as usize;
                        }
                    }
                    page += n_threads;
                }
                checksum.fetch_xor(local, Ordering::Relaxed);
            });
        }
    });

    std::hint::black_box(checksum.load(Ordering::Relaxed));
}

pub struct MultiSafetensors {
    pub shards: Vec<SafetensorsFile>,
}

impl MultiSafetensors {
    pub fn open(model_dir: &str) -> Option<Self> {
        // Try single file first
        let single_path = format!("{}/model.safetensors", model_dir);
        if let Some(sf) = SafetensorsFile::open(&single_path) {
            return Some(MultiSafetensors { shards: vec![sf] });
        }

        // Scan directory for shard files
        let dir = Path::new(model_dir);
        let mut shard_names: Vec<String> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("model-") && name.ends_with(".safetensors") {
                    shard_names.push(name);
                }
            }
        }

        if shard_names.is_empty() {
            eprintln!(
                "multi_safetensors_open: no safetensors files in {}",
                model_dir
            );
            return None;
        }

        shard_names.sort();

        let mut shards = Vec::new();
        for name in &shard_names {
            let path = format!("{}/{}", model_dir, name);
            match SafetensorsFile::open(&path) {
                Some(sf) => shards.push(sf),
                None => {
                    eprintln!("multi_safetensors_open: failed to open {}", path);
                    return None;
                }
            }
        }

        Some(MultiSafetensors { shards })
    }

    /// Find a tensor by name across all shards.
    /// Returns (shard_index, TensorMeta).
    pub fn find(&self, name: &str) -> Option<(usize, &TensorMeta)> {
        for (si, shard) in self.shards.iter().enumerate() {
            if let Some(t) = shard.find(name) {
                return Some((si, t));
            }
        }
        None
    }

    /// Convenience: get f32 data for a named tensor.
    pub fn get_f32(&self, name: &str) -> Option<Vec<f32>> {
        let (si, t) = self.find(name)?;
        self.shards[si].get_f32(t)
    }

    /// Convenience: get direct BF16 pointer for a named tensor.
    pub fn get_bf16_direct(&self, name: &str) -> Option<*const u16> {
        let (si, t) = self.find(name)?;
        self.shards[si].get_bf16_direct(t)
    }

    /// Check if a tensor exists.
    pub fn has_tensor(&self, name: &str) -> bool {
        self.find(name).is_some()
    }
}

// ========================================================================
// Minimal JSON parser for safetensors header
// ========================================================================

fn parse_header(json: &str) -> Option<Vec<TensorMeta>> {
    let mut tensors = Vec::new();
    let bytes = json.as_bytes();
    let mut pos = 0;

    skip_whitespace(bytes, &mut pos);
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return None;
    }
    pos += 1;

    loop {
        skip_whitespace(bytes, &mut pos);
        if pos >= bytes.len() {
            break;
        }
        if bytes[pos] == b'}' {
            break;
        }
        if bytes[pos] == b',' {
            pos += 1;
            continue;
        }

        // Parse key
        let key = parse_json_string(bytes, &mut pos)?;
        skip_whitespace(bytes, &mut pos);
        if pos >= bytes.len() || bytes[pos] != b':' {
            return None;
        }
        pos += 1;

        if key == "__metadata__" {
            skip_json_value(bytes, &mut pos);
            continue;
        }

        // Parse tensor entry
        let tensor = parse_tensor_entry(bytes, &mut pos, &key)?;
        tensors.push(tensor);

        if tensors.len() >= MAX_TENSORS {
            break;
        }
    }

    Some(tensors)
}

fn skip_whitespace(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() {
        match bytes[*pos] {
            b' ' | b'\n' | b'\r' | b'\t' => *pos += 1,
            _ => break,
        }
    }
}

fn parse_json_string(bytes: &[u8], pos: &mut usize) -> Option<String> {
    skip_whitespace(bytes, pos);
    if *pos >= bytes.len() || bytes[*pos] != b'"' {
        return None;
    }
    *pos += 1;

    let mut result = Vec::new();
    while *pos < bytes.len() && bytes[*pos] != b'"' {
        if bytes[*pos] == b'\\' {
            *pos += 1;
            if *pos >= bytes.len() {
                return None;
            }
            match bytes[*pos] {
                b'n' => result.push(b'\n'),
                b't' => result.push(b'\t'),
                b'"' => result.push(b'"'),
                b'\\' => result.push(b'\\'),
                b'/' => result.push(b'/'),
                b'u' => {
                    *pos += 1;
                    let mut cp = 0u32;
                    for _ in 0..4 {
                        if *pos >= bytes.len() {
                            return None;
                        }
                        cp <<= 4;
                        let c = bytes[*pos];
                        cp |= match c {
                            b'0'..=b'9' => (c - b'0') as u32,
                            b'a'..=b'f' => (c - b'a' + 10) as u32,
                            b'A'..=b'F' => (c - b'A' + 10) as u32,
                            _ => return None,
                        };
                        *pos += 1;
                    }
                    // Encode cp as UTF-8
                    let ch = char::from_u32(cp).unwrap_or('?');
                    let mut buf = [0u8; 4];
                    let s = ch.encode_utf8(&mut buf);
                    result.extend_from_slice(s.as_bytes());
                    continue; // skip the pos += 1 below
                }
                other => result.push(other),
            }
        } else {
            result.push(bytes[*pos]);
        }
        *pos += 1;
    }

    if *pos >= bytes.len() || bytes[*pos] != b'"' {
        return None;
    }
    *pos += 1;

    String::from_utf8(result).ok()
}

fn parse_json_int(bytes: &[u8], pos: &mut usize) -> Option<i64> {
    skip_whitespace(bytes, pos);
    let mut neg = false;
    if *pos < bytes.len() && bytes[*pos] == b'-' {
        neg = true;
        *pos += 1;
    }
    let mut val: i64 = 0;
    let mut found = false;
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
        val = val * 10 + (bytes[*pos] - b'0') as i64;
        *pos += 1;
        found = true;
    }
    if !found {
        return None;
    }
    Some(if neg { -val } else { val })
}

fn parse_tensor_entry(bytes: &[u8], pos: &mut usize, name: &str) -> Option<TensorMeta> {
    skip_whitespace(bytes, pos);
    if *pos >= bytes.len() || bytes[*pos] != b'{' {
        return None;
    }
    *pos += 1;

    let mut dtype = Dtype::Unknown;
    let mut shape = Vec::new();
    let mut data_offset: usize = 0;
    let mut data_size: usize = 0;

    loop {
        skip_whitespace(bytes, pos);
        if *pos >= bytes.len() {
            break;
        }
        if bytes[*pos] == b'}' {
            *pos += 1;
            break;
        }
        if bytes[*pos] == b',' {
            *pos += 1;
            continue;
        }

        let key = parse_json_string(bytes, pos)?;
        skip_whitespace(bytes, pos);
        if *pos >= bytes.len() || bytes[*pos] != b':' {
            return None;
        }
        *pos += 1;
        skip_whitespace(bytes, pos);

        match key.as_str() {
            "dtype" => {
                let dtype_str = parse_json_string(bytes, pos)?;
                dtype = Dtype::from_str(&dtype_str);
            }
            "shape" => {
                if *pos >= bytes.len() || bytes[*pos] != b'[' {
                    return None;
                }
                *pos += 1;
                loop {
                    skip_whitespace(bytes, pos);
                    if *pos >= bytes.len() {
                        break;
                    }
                    if bytes[*pos] == b']' {
                        *pos += 1;
                        break;
                    }
                    if bytes[*pos] == b',' {
                        *pos += 1;
                        continue;
                    }
                    let dim = parse_json_int(bytes, pos)?;
                    shape.push(dim);
                }
            }
            "data_offsets" => {
                if *pos >= bytes.len() || bytes[*pos] != b'[' {
                    return None;
                }
                *pos += 1;
                skip_whitespace(bytes, pos);
                let start = parse_json_int(bytes, pos)? as usize;
                skip_whitespace(bytes, pos);
                if *pos < bytes.len() && bytes[*pos] == b',' {
                    *pos += 1;
                }
                skip_whitespace(bytes, pos);
                let end = parse_json_int(bytes, pos)? as usize;
                skip_whitespace(bytes, pos);
                if *pos < bytes.len() && bytes[*pos] == b']' {
                    *pos += 1;
                }
                data_offset = start;
                data_size = end - start;
            }
            _ => {
                skip_json_value(bytes, pos);
            }
        }
    }

    Some(TensorMeta {
        name: name.to_string(),
        dtype,
        shape,
        data_offset,
        data_size,
    })
}

fn skip_json_value(bytes: &[u8], pos: &mut usize) {
    skip_whitespace(bytes, pos);
    if *pos >= bytes.len() {
        return;
    }

    match bytes[*pos] {
        b'"' => {
            *pos += 1;
            while *pos < bytes.len() && bytes[*pos] != b'"' {
                if bytes[*pos] == b'\\' {
                    *pos += 1;
                }
                if *pos < bytes.len() {
                    *pos += 1;
                }
            }
            if *pos < bytes.len() {
                *pos += 1;
            }
        }
        b'[' => {
            let mut depth = 1;
            *pos += 1;
            while *pos < bytes.len() && depth > 0 {
                match bytes[*pos] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    b'"' => {
                        *pos += 1;
                        while *pos < bytes.len() && bytes[*pos] != b'"' {
                            if bytes[*pos] == b'\\' {
                                *pos += 1;
                            }
                            if *pos < bytes.len() {
                                *pos += 1;
                            }
                        }
                    }
                    _ => {}
                }
                *pos += 1;
            }
        }
        b'{' => {
            let mut depth = 1;
            *pos += 1;
            while *pos < bytes.len() && depth > 0 {
                match bytes[*pos] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    b'"' => {
                        *pos += 1;
                        while *pos < bytes.len() && bytes[*pos] != b'"' {
                            if bytes[*pos] == b'\\' {
                                *pos += 1;
                            }
                            if *pos < bytes.len() {
                                *pos += 1;
                            }
                        }
                    }
                    _ => {}
                }
                *pos += 1;
            }
        }
        _ => {
            // Number, bool, null
            while *pos < bytes.len()
                && bytes[*pos] != b','
                && bytes[*pos] != b'}'
                && bytes[*pos] != b']'
            {
                *pos += 1;
            }
        }
    }
}
