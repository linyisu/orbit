//! Bounded Camera2 capture for QR recognition. Only the luma plane leaves the camera worker.
use super::Scan;
use ndk_sys::*;
use std::{
    ptr,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::sync::mpsc::Sender;

#[link(name = "camera2ndk")]
unsafe extern "C" {}
static GENERATION: AtomicU64 = AtomicU64::new(0);
static CAMERA: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn stop() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn disconnected(_: *mut std::ffi::c_void, _: *mut ACameraDevice) {
    stop();
}
unsafe extern "C" fn failed(_: *mut std::ffi::c_void, _: *mut ACameraDevice, _: i32) {
    stop();
}
unsafe extern "C" fn session_state(_: *mut std::ffi::c_void, _: *mut ACameraCaptureSession) {}

#[derive(Default)]
struct Camera {
    rotation: i32,
    manager: *mut ACameraManager,
    device: *mut ACameraDevice,
    reader: *mut AImageReader,
    outputs: *mut ACaptureSessionOutputContainer,
    output: *mut ACaptureSessionOutput,
    target: *mut ACameraOutputTarget,
    request: *mut ACaptureRequest,
    session: *mut ACameraCaptureSession,
}
impl Drop for Camera {
    fn drop(&mut self) {
        // SAFETY: all handles are created on this worker and released in dependency order.
        unsafe {
            if !self.session.is_null() {
                ACameraCaptureSession_stopRepeating(self.session);
                ACameraCaptureSession_close(self.session);
            }
            if !self.device.is_null() {
                ACameraDevice_close(self.device);
            }
            if !self.request.is_null() {
                ACaptureRequest_free(self.request);
            }
            if !self.target.is_null() {
                ACameraOutputTarget_free(self.target);
            }
            if !self.outputs.is_null() {
                ACaptureSessionOutputContainer_free(self.outputs);
            }
            if !self.output.is_null() {
                ACaptureSessionOutput_free(self.output);
            }
            if !self.reader.is_null() {
                AImageReader_delete(self.reader);
            }
            if !self.manager.is_null() {
                ACameraManager_delete(self.manager);
            }
        }
    }
}
fn check(status: camera_status_t) -> Result<(), String> {
    if status.0 == 0 {
        Ok(())
    } else {
        Err(format!(
            "无法打开相机（{}），请关闭其他相机应用后重试",
            status.0
        ))
    }
}
impl Camera {
    fn open() -> Result<Self, String> {
        let mut camera = Self::default();
        // SAFETY: each output points to initialized writable storage; successful handles stay alive
        // in Camera until capture ends. Callbacks use no borrowed context and outlive the session.
        unsafe {
            camera.manager = ACameraManager_create();
            if camera.manager.is_null() {
                return Err("相机不可用".into());
            }
            let mut ids = ptr::null_mut();
            check(ACameraManager_getCameraIdList(camera.manager, &mut ids))?;
            if ids.is_null() || (*ids).numCameras == 0 {
                if !ids.is_null() {
                    ACameraManager_deleteCameraIdList(ids);
                }
                return Err("没有可用相机".into());
            }
            let mut selected = *(*ids).cameraIds;
            let mut size = (640, 480);
            let mut autofocus = false;
            for index in 0..(*ids).numCameras as usize {
                let id = *(*ids).cameraIds.add(index);
                let mut metadata = ptr::null_mut();
                if ACameraManager_getCameraCharacteristics(camera.manager, id, &mut metadata).0 != 0
                    || metadata.is_null()
                {
                    continue;
                }
                let mut entry: ACameraMetadata_const_entry = std::mem::zeroed();
                let back = ACameraMetadata_getConstEntry(
                    metadata,
                    acamera_metadata_tag::ACAMERA_LENS_FACING.0,
                    &mut entry,
                )
                .0 == 0
                    && entry.count > 0
                    && *entry.data.u8_ == 1;
                if index == 0 || back {
                    selected = id;
                    if ACameraMetadata_getConstEntry(
                        metadata,
                        acamera_metadata_tag::ACAMERA_SENSOR_ORIENTATION.0,
                        &mut entry,
                    )
                    .0 == 0
                        && entry.count > 0
                    {
                        camera.rotation = *entry.data.i32_;
                    }
                    if ACameraMetadata_getConstEntry(
                        metadata,
                        acamera_metadata_tag::ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS.0,
                        &mut entry,
                    )
                    .0 == 0
                    {
                        let configurations =
                            std::slice::from_raw_parts(entry.data.i32_, entry.count as usize);
                        if let Some(configuration) = configurations
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .filter(|c| {
                                c[0] == 35
                                    && c[3] == 0
                                    && c[1] > 0
                                    && c[2] > 0
                                    && c[1] <= 1920
                                    && c[2] <= 1920
                            })
                            .min_by_key(|c| (i64::from(c[1]) * i64::from(c[2]) - 1280 * 720).abs())
                        {
                            size = (configuration[1], configuration[2]);
                        }
                    }
                    autofocus = ACameraMetadata_getConstEntry(
                        metadata,
                        acamera_metadata_tag::ACAMERA_CONTROL_AF_AVAILABLE_MODES.0,
                        &mut entry,
                    )
                    .0 == 0
                        && std::slice::from_raw_parts(entry.data.u8_, entry.count as usize)
                            .contains(&4);
                }
                ACameraMetadata_free(metadata);
                if back {
                    break;
                }
            }
            let mut callbacks = ACameraDevice_StateCallbacks {
                context: ptr::null_mut(),
                onDisconnected: Some(disconnected),
                onError: Some(failed),
            };
            let opened = ACameraManager_openCamera(
                camera.manager,
                selected,
                &mut callbacks,
                &mut camera.device,
            );
            ACameraManager_deleteCameraIdList(ids);
            check(opened)?;
            if AImageReader_new(
                size.0,
                size.1,
                AIMAGE_FORMATS::AIMAGE_FORMAT_YUV_420_888.0 as i32,
                3,
                &mut camera.reader,
            )
            .0 != 0
            {
                return Err("相机不支持扫码预览".into());
            }
            let mut window = ptr::null_mut();
            if AImageReader_getWindow(camera.reader, &mut window).0 != 0 {
                return Err("无法创建相机预览".into());
            }
            check(ACaptureSessionOutputContainer_create(&mut camera.outputs))?;
            check(ACaptureSessionOutput_create(window, &mut camera.output))?;
            check(ACaptureSessionOutputContainer_add(
                camera.outputs,
                camera.output,
            ))?;
            check(ACameraOutputTarget_create(window, &mut camera.target))?;
            check(ACameraDevice_createCaptureRequest(
                camera.device,
                ACameraDevice_request_template::TEMPLATE_PREVIEW,
                &mut camera.request,
            ))?;
            check(ACaptureRequest_addTarget(camera.request, camera.target))?;
            if autofocus {
                check(ACaptureRequest_setEntry_u8(
                    camera.request,
                    acamera_metadata_tag::ACAMERA_CONTROL_AF_MODE.0,
                    1,
                    &4,
                ))?;
            }
            let callbacks = ACameraCaptureSession_stateCallbacks {
                context: ptr::null_mut(),
                onClosed: Some(session_state),
                onReady: Some(session_state),
                onActive: Some(session_state),
            };
            check(ACameraDevice_createCaptureSession(
                camera.device,
                camera.outputs,
                &callbacks,
                &mut camera.session,
            ))?;
            check(ACameraCaptureSession_setRepeatingRequest(
                camera.session,
                ptr::null_mut(),
                1,
                &mut camera.request,
                ptr::null_mut(),
            ))?;
        }
        Ok(camera)
    }
    fn frame(&self) -> Option<image::GrayImage> {
        // SAFETY: reader belongs to this live camera; the acquired image is retained while its
        // plane is copied. Row/pixel strides and plane length bound every read, then it is deleted.
        unsafe {
            let mut raw = ptr::null_mut();
            if AImageReader_acquireLatestImage(self.reader, &mut raw).0 != 0 || raw.is_null() {
                return None;
            }
            let result = (|| {
                let (mut width, mut height, mut row, mut stride, mut len) = (0, 0, 0, 0, 0);
                let mut bytes = ptr::null_mut();
                if AImage_getWidth(raw, &mut width).0 != 0
                    || AImage_getHeight(raw, &mut height).0 != 0
                    || AImage_getPlaneRowStride(raw, 0, &mut row).0 != 0
                    || AImage_getPlanePixelStride(raw, 0, &mut stride).0 != 0
                    || AImage_getPlaneData(raw, 0, &mut bytes, &mut len).0 != 0
                {
                    return None;
                }
                if width <= 0
                    || height <= 0
                    || width > 4096
                    || height > 4096
                    || row <= 0
                    || stride <= 0
                    || len <= 0
                    || bytes.is_null()
                {
                    return None;
                }
                let plane = std::slice::from_raw_parts(bytes, len as usize);
                if ((height - 1) as usize * row as usize + (width - 1) as usize * stride as usize)
                    >= plane.len()
                {
                    return None;
                }
                Some(image::GrayImage::from_fn(
                    width as u32,
                    height as u32,
                    |x, y| {
                        image::Luma([
                            plane[y as usize * row as usize + x as usize * stride as usize]
                        ])
                    },
                ))
            })();
            AImage_delete(raw);
            result
        }
    }
}
pub fn scan(sender: Sender<Result<Scan, String>>) {
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let _guard = CAMERA.lock().unwrap();
    let camera = match Camera::open() {
        Ok(camera) => camera,
        Err(error) => {
            let _ = sender.blocking_send(Err(error));
            return;
        }
    };
    let started = Instant::now();
    while !sender.is_closed()
        && GENERATION.load(Ordering::Relaxed) == generation
        && started.elapsed() < Duration::from_secs(120)
    {
        if let Some(frame) = camera.frame() {
            if let Some(code) = crate::platform::decode(frame.clone()) {
                let _ = sender.blocking_send(Ok(Scan::Code(code)));
                return;
            }
            let preview = match camera.rotation {
                90 => image::imageops::rotate90(&frame),
                180 => image::imageops::rotate180(&frame),
                270 => image::imageops::rotate270(&frame),
                _ => frame,
            };
            let mut bytes = std::io::Cursor::new(Vec::new());
            if image::DynamicImage::ImageLuma8(preview)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .is_ok()
            {
                let _ = sender.try_send(Ok(Scan::Preview(bytes.into_inner())));
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    if !sender.is_closed() {
        let _ = sender.blocking_send(Err("扫码已停止，请重试".into()));
    }
}
