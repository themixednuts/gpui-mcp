use std::ffi::OsStr;
use std::fs;
use std::io::{self, Cursor, Seek, SeekFrom, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use bytes::Bytes;
use gpui_mcp_protocol::Screenshot;
use image::{ImageFormat, ImageReader, Limits, RgbaImage};
use mp4::{AvcConfig, FourCC, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig};
use openh264::{
    OpenH264API,
    encoder::{Encoder, EncoderConfig, FrameType, RateControlMode, UsageType},
    formats::{RgbaSliceU8, YUVBuffer},
};
use tempfile::NamedTempFile;

#[cfg(test)]
pub(crate) const DEFAULT_FRAME_DELAY_MS: u32 = 100;
pub(crate) const MIN_FRAME_DELAY_MS: u32 = 20;
pub(crate) const MAX_FRAME_DELAY_MS: u32 = 10_000;
pub(crate) const MAX_RECORDING_FRAMES: usize = 120;
const MAX_CAPTURE_FAILURES: u8 = 3;
const MAX_ARTIFACT_NAME_BYTES: usize = 128;
const MAX_RECORDING_FRAME_DIMENSION: u32 = 4_096;
const MAX_RECORDING_FRAME_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_RECORDING_FRAME_BASE64_BYTES: u64 = 24 * 1024 * 1024;
const MAX_RECORDING_FRAME_PNG_BYTES: usize = 16 * 1024 * 1024;
const MAX_RECORDING_BASE64_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORDING_DECODED_PIXELS: u64 = 256 * 1024 * 1024;
const MAX_RECORDING_OUTPUT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct ArtifactStore {
    directory: Arc<PathBuf>,
}

impl ArtifactStore {
    pub(crate) fn open(directory: impl AsRef<Path>) -> Result<Self, String> {
        let directory = directory.as_ref();
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "recording artifact directory {} must not be a symlink",
                    directory.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "recording artifact directory {} is not a directory",
                    directory.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(directory).map_err(|error| {
                    format!(
                        "could not create recording artifact directory {}: {error}",
                        directory.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "could not inspect recording artifact directory {}: {error}",
                    directory.display()
                ));
            }
        }

        let metadata = fs::symlink_metadata(directory).map_err(|error| {
            format!(
                "could not inspect recording artifact directory {}: {error}",
                directory.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "recording artifact directory {} must be a real directory",
                directory.display()
            ));
        }
        #[cfg(unix)]
        secure_directory_permissions(directory)?;
        let directory = fs::canonicalize(directory).map_err(|error| {
            format!(
                "could not resolve recording artifact directory {}: {error}",
                directory.display()
            )
        })?;
        Ok(Self {
            directory: Arc::new(directory),
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        self.directory.as_path()
    }

    fn encode_mp4(
        &self,
        frames: &[Screenshot],
        artifact_name: &str,
        delay_ms: u32,
        overwrite: bool,
    ) -> Result<RecordingArtifact, String> {
        validate_artifact_name(artifact_name)?;
        validate_frame_delay(delay_ms)?;
        if frames.is_empty() {
            return Err("no frames were captured for this recording".to_owned());
        }
        if frames.len() > MAX_RECORDING_FRAMES {
            return Err(format!(
                "recording exceeds the {MAX_RECORDING_FRAMES}-frame safety bound"
            ));
        }

        let (dimensions, _base64_bytes, _decoded_pixels) = inspect_frames(frames)?;
        let (video_width, video_height) = padded_video_dimensions(dimensions)?;

        let final_path = self.directory.join(artifact_name);
        validate_existing_artifact(&final_path, overwrite)?;
        let mut temporary = NamedTempFile::new_in(self.directory()).map_err(|error| {
            format!(
                "could not create a temporary recording artifact in {}: {error}",
                self.directory().display()
            )
        })?;

        let encoded_bytes = {
            encode_h264_mp4(
                temporary.as_file_mut(),
                frames,
                dimensions,
                (video_width, video_height),
                delay_ms,
            )?
        };
        install_artifact(
            temporary,
            &final_path,
            self.directory(),
            artifact_name,
            encoded_bytes,
            overwrite,
        )?;

        Ok(RecordingArtifact {
            path: final_path,
            frame_count: frames.len(),
            delay_ms,
            bytes: encoded_bytes,
            width: video_width,
            height: video_height,
            dropped_frames: 0,
        })
    }
}

fn install_artifact(
    temporary: NamedTempFile,
    final_path: &Path,
    directory: &Path,
    artifact_name: &str,
    encoded_bytes: u64,
    overwrite: bool,
) -> Result<(), String> {
    #[cfg(not(unix))]
    let _ = directory;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("could not synchronize MP4 data: {error}"))?;
    let metadata = temporary
        .as_file()
        .metadata()
        .map_err(|error| format!("could not inspect temporary recording artifact: {error}"))?;
    if !metadata.is_file()
        || metadata.len() > MAX_RECORDING_OUTPUT_BYTES
        || metadata.len() != encoded_bytes
    {
        return Err("temporary recording artifact has an invalid size or type".to_owned());
    }

    // Keep every fallible validation before the atomic install. Once persist succeeds the
    // artifact is committed, so returning an error would incorrectly restore frames for a retry
    // even though the destination already contains the completed recording.
    drop(
        if overwrite {
            temporary.persist(final_path)
        } else {
            temporary.persist_noclobber(final_path)
        }
        .map_err(|error| {
            if !overwrite && error.error.kind() == io::ErrorKind::AlreadyExists {
                format!(
                    "recording artifact {artifact_name:?} already exists; pass overwrite=true to replace a regular file"
                )
            } else {
                format!(
                    "could not atomically install recording artifact {artifact_name:?}: {}",
                    error.error
                )
            }
        })?,
    );
    #[cfg(unix)]
    if let Err(error) = sync_parent_directory(directory) {
        tracing::warn!(
            %error,
            artifact = %final_path.display(),
            "could not synchronize committed recording artifact directory"
        );
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct RecordingState {
    last_session_id: u64,
    phase: RecordingPhase,
}

impl Default for RecordingState {
    fn default() -> Self {
        Self {
            last_session_id: 0,
            phase: RecordingPhase::Idle,
        }
    }
}

#[derive(Debug)]
enum RecordingPhase {
    Idle,
    Recording(RecordingSession),
    Encoding { session_id: u64 },
}

#[derive(Debug)]
struct RecordingSession {
    session_id: u64,
    frames: Vec<Screenshot>,
    base64_bytes: u64,
    decoded_pixels: u64,
    dimensions: Option<(u32, u32)>,
    capture_in_flight: bool,
    pointer_overlay: bool,
    frame_duration_ms: u32,
    dropped_frames: u32,
    last_capture_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CapturePermit {
    session_id: u64,
    pointer_overlay: bool,
}

pub(crate) struct CaptureLease {
    state: Arc<Mutex<RecordingState>>,
    permit: Option<CapturePermit>,
}

impl CaptureLease {
    pub(crate) fn begin(state: Arc<Mutex<RecordingState>>) -> Result<Self, String> {
        let permit = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_capture()?;
        Ok(Self {
            state,
            permit: Some(permit),
        })
    }

    pub(crate) fn complete(mut self, screenshot: Screenshot) -> Result<CaptureOutcome, String> {
        let permit = self
            .permit
            .ok_or_else(|| "recording capture state was already finalized".to_owned())?;
        let outcome = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .complete_capture(permit, screenshot);
        self.permit = None;
        outcome
    }

    #[must_use]
    pub(crate) fn includes_pointer_overlay(&self) -> bool {
        self.permit.is_some_and(|permit| permit.pointer_overlay)
    }
}

impl Drop for CaptureLease {
    fn drop(&mut self) {
        let Some(permit) = self.permit.take() else {
            return;
        };
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .abort_capture(permit);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CaptureOutcome {
    pub(crate) session_id: u64,
    pub(crate) frame_count: usize,
    pub(crate) base64_bytes: u64,
    pub(crate) decoded_pixels: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Debug)]
pub(crate) struct EncodingJob {
    session: RecordingSession,
}

impl EncodingJob {
    pub(crate) fn session_id(&self) -> u64 {
        self.session.session_id
    }

    pub(crate) fn frame_duration_ms(&self) -> u32 {
        self.session.frame_duration_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordingArtifact {
    pub(crate) path: PathBuf,
    pub(crate) frame_count: usize,
    pub(crate) delay_ms: u32,
    pub(crate) bytes: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) dropped_frames: u32,
}

impl RecordingState {
    pub(crate) const fn is_idle(&self) -> bool {
        matches!(self.phase, RecordingPhase::Idle)
    }

    #[cfg(test)]
    pub(crate) fn start(&mut self) -> Result<u64, String> {
        self.start_with_config(true, DEFAULT_FRAME_DELAY_MS)
    }

    pub(crate) fn start_with_config(
        &mut self,
        pointer_overlay: bool,
        frame_duration_ms: u32,
    ) -> Result<u64, String> {
        validate_frame_delay(frame_duration_ms)?;
        match self.phase {
            RecordingPhase::Idle => {}
            RecordingPhase::Recording(_) => {
                return Err("a recording session is already active".to_owned());
            }
            RecordingPhase::Encoding { .. } => {
                return Err("recording encoding is still in progress".to_owned());
            }
        }
        let session_id = self
            .last_session_id
            .checked_add(1)
            .ok_or_else(|| "recording session identifier capacity was exhausted".to_owned())?;
        self.last_session_id = session_id;
        self.phase = RecordingPhase::Recording(RecordingSession {
            session_id,
            frames: Vec::with_capacity(MAX_RECORDING_FRAMES.min(16)),
            base64_bytes: 0,
            decoded_pixels: 0,
            dimensions: None,
            capture_in_flight: false,
            pointer_overlay,
            frame_duration_ms,
            dropped_frames: 0,
            last_capture_error: None,
        });
        Ok(session_id)
    }

    pub(crate) fn note_continuous_capture_failure(
        &mut self,
        session_id: u64,
        error: String,
    ) -> bool {
        let RecordingPhase::Recording(session) = &mut self.phase else {
            return false;
        };
        if session.session_id != session_id {
            return false;
        }
        session.dropped_frames = session.dropped_frames.saturating_add(1);
        session.last_capture_error = Some(error);
        session.dropped_frames < u32::from(MAX_CAPTURE_FAILURES)
    }

    pub(crate) fn begin_capture(&mut self) -> Result<CapturePermit, String> {
        let RecordingPhase::Recording(session) = &mut self.phase else {
            return Err(match self.phase {
                RecordingPhase::Idle => {
                    "no active recording; call start_video_recording first".to_owned()
                }
                RecordingPhase::Encoding { .. } => {
                    "recording encoding is in progress; no frame can be captured".to_owned()
                }
                RecordingPhase::Recording(_) => "recording state is unavailable".to_owned(),
            });
        };
        if session.capture_in_flight {
            return Err("a recording frame capture is already in progress".to_owned());
        }
        if session.frames.len() >= MAX_RECORDING_FRAMES {
            return Err(format!(
                "recording frame capacity ({MAX_RECORDING_FRAMES}) has been reached"
            ));
        }
        if session.base64_bytes >= MAX_RECORDING_BASE64_BYTES {
            return Err("recording stored-base64 capacity has been reached".to_owned());
        }
        if session.decoded_pixels >= MAX_RECORDING_DECODED_PIXELS {
            return Err("recording decoded-pixel capacity has been reached".to_owned());
        }
        session.capture_in_flight = true;
        Ok(CapturePermit {
            session_id: session.session_id,
            pointer_overlay: session.pointer_overlay,
        })
    }

    pub(crate) fn abort_capture(&mut self, permit: CapturePermit) {
        if let RecordingPhase::Recording(session) = &mut self.phase
            && session.session_id == permit.session_id
        {
            session.capture_in_flight = false;
        }
    }

    pub(crate) fn complete_capture(
        &mut self,
        permit: CapturePermit,
        screenshot: Screenshot,
    ) -> Result<CaptureOutcome, String> {
        let RecordingPhase::Recording(session) = &mut self.phase else {
            return Err("the recording session changed while its frame was captured".to_owned());
        };
        if session.session_id != permit.session_id || !session.capture_in_flight {
            return Err("the recording session changed while its frame was captured".to_owned());
        }
        session.capture_in_flight = false;
        let usage = validate_frame(&screenshot, session.dimensions)?;
        let base64_bytes = checked_total(
            session.base64_bytes,
            usage.base64_bytes,
            MAX_RECORDING_BASE64_BYTES,
            "stored base64 data",
        )?;
        let decoded_pixels = checked_total(
            session.decoded_pixels,
            usage.pixels,
            MAX_RECORDING_DECODED_PIXELS,
            "decoded pixels",
        )?;
        session.base64_bytes = base64_bytes;
        session.decoded_pixels = decoded_pixels;
        session.dimensions.get_or_insert(usage.dimensions);
        session.frames.push(screenshot);
        Ok(CaptureOutcome {
            session_id: session.session_id,
            frame_count: session.frames.len(),
            base64_bytes,
            decoded_pixels,
            width: usage.dimensions.0,
            height: usage.dimensions.1,
        })
    }

    pub(crate) fn begin_encoding(&mut self) -> Result<EncodingJob, String> {
        let phase = std::mem::replace(&mut self.phase, RecordingPhase::Idle);
        match phase {
            RecordingPhase::Idle => {
                Err("no active recording; call start_video_recording first".to_owned())
            }
            RecordingPhase::Encoding { session_id } => {
                self.phase = RecordingPhase::Encoding { session_id };
                Err("recording encoding is already in progress".to_owned())
            }
            RecordingPhase::Recording(session) if session.capture_in_flight => {
                self.phase = RecordingPhase::Recording(session);
                Err("a recording frame capture is still in progress".to_owned())
            }
            RecordingPhase::Recording(session) if session.frames.is_empty() => {
                let message = session.last_capture_error.as_ref().map_or_else(
                    || "no video frames were captured for this recording".to_owned(),
                    |error| format!("no video frames were captured; latest capture error: {error}"),
                );
                self.phase = RecordingPhase::Recording(session);
                Err(message)
            }
            RecordingPhase::Recording(session) => {
                let session_id = session.session_id;
                self.phase = RecordingPhase::Encoding { session_id };
                Ok(EncodingJob { session })
            }
        }
    }

    fn finish_encoding(&mut self, session_id: u64) -> bool {
        if matches!(
            self.phase,
            RecordingPhase::Encoding {
                session_id: current
            } if current == session_id
        ) {
            self.phase = RecordingPhase::Idle;
            true
        } else {
            false
        }
    }

    fn restore_encoding(&mut self, mut session: RecordingSession) {
        if matches!(
            self.phase,
            RecordingPhase::Encoding {
                session_id: current
            } if current == session.session_id
        ) {
            session.capture_in_flight = false;
            self.phase = RecordingPhase::Recording(session);
        }
    }
}

pub(crate) fn run_encoding_job(
    state: Arc<Mutex<RecordingState>>,
    store: &ArtifactStore,
    job: EncodingJob,
    artifact_name: &str,
    delay_ms: u32,
    overwrite: bool,
) -> Result<RecordingArtifact, String> {
    let mut guard = EncodingGuard::new(state, job);
    let mut artifact = store.encode_mp4(guard.frames(), artifact_name, delay_ms, overwrite)?;
    artifact.dropped_frames = guard.dropped_frames();
    guard.commit()?;
    Ok(artifact)
}

pub(crate) fn validate_artifact_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_ARTIFACT_NAME_BYTES {
        return Err(format!(
            "artifact_name must contain 1 through {MAX_ARTIFACT_NAME_BYTES} ASCII bytes"
        ));
    }
    if Path::new(name).extension() != Some(OsStr::new("mp4")) {
        return Err("artifact_name must end with lowercase .mp4".to_owned());
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || !name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(
            "artifact_name may contain only ASCII letters, digits, '.', '_', and '-', and must start with a letter or digit"
                .to_owned(),
        );
    }
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(component)) if component == OsStr::new(name))
        || components.next().is_some()
    {
        return Err("artifact_name must be one filename without path separators".to_owned());
    }
    let stem = name.strip_suffix(".mp4").unwrap_or_default();
    if stem.is_empty() || is_windows_device_name(stem) {
        return Err("artifact_name is not a portable regular filename".to_owned());
    }
    Ok(())
}

pub(crate) fn validate_frame_delay(delay_ms: u32) -> Result<(), String> {
    if (MIN_FRAME_DELAY_MS..=MAX_FRAME_DELAY_MS).contains(&delay_ms) {
        Ok(())
    } else {
        Err(format!(
            "frame_duration_ms must be between {MIN_FRAME_DELAY_MS} and {MAX_FRAME_DELAY_MS}"
        ))
    }
}

struct EncodingGuard {
    state: Arc<Mutex<RecordingState>>,
    job: Option<EncodingJob>,
}

impl EncodingGuard {
    fn new(state: Arc<Mutex<RecordingState>>, job: EncodingJob) -> Self {
        Self {
            state,
            job: Some(job),
        }
    }

    fn frames(&self) -> &[Screenshot] {
        self.job
            .as_ref()
            .map_or(&[], |job| job.session.frames.as_slice())
    }

    fn dropped_frames(&self) -> u32 {
        self.job
            .as_ref()
            .map_or(0, |job| job.session.dropped_frames)
    }

    fn commit(&mut self) -> Result<(), String> {
        let Some(job) = self.job.as_ref() else {
            return Err("recording encoding state was already finalized".to_owned());
        };
        let session_id = job.session.session_id;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.finish_encoding(session_id) {
            return Err("recording encoding state changed unexpectedly".to_owned());
        }
        drop(state);
        self.job = None;
        Ok(())
    }
}

impl Drop for EncodingGuard {
    fn drop(&mut self) {
        let Some(job) = self.job.take() else {
            return;
        };
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .restore_encoding(job.session);
    }
}

#[derive(Clone, Copy)]
struct FrameUsage {
    dimensions: (u32, u32),
    pixels: u64,
    base64_bytes: u64,
}

fn validate_frame(
    screenshot: &Screenshot,
    expected_dimensions: Option<(u32, u32)>,
) -> Result<FrameUsage, String> {
    if screenshot.mime_type != "image/png" {
        return Err("recording frames must be PNG screenshots".to_owned());
    }
    if screenshot.width == 0
        || screenshot.height == 0
        || screenshot.width > MAX_RECORDING_FRAME_DIMENSION
        || screenshot.height > MAX_RECORDING_FRAME_DIMENSION
    {
        return Err(format!(
            "recording frame dimensions must be between 1 and {MAX_RECORDING_FRAME_DIMENSION} pixels per axis"
        ));
    }
    let dimensions = (screenshot.width, screenshot.height);
    if expected_dimensions.is_some_and(|expected| expected != dimensions) {
        return Err(format!(
            "recording frame dimensions {}x{} do not match the first frame",
            screenshot.width, screenshot.height
        ));
    }
    let pixels = u64::from(screenshot.width)
        .checked_mul(u64::from(screenshot.height))
        .ok_or_else(|| "recording frame pixel count overflowed".to_owned())?;
    if pixels > MAX_RECORDING_FRAME_PIXELS {
        return Err("recording frame exceeds the per-frame pixel safety bound".to_owned());
    }
    let base64_bytes = u64::try_from(screenshot.base64_data.len())
        .map_err(|_| "recording frame base64 size overflowed".to_owned())?;
    if base64_bytes == 0 || base64_bytes > MAX_RECORDING_FRAME_BASE64_BYTES {
        return Err("recording frame exceeds the per-frame base64 safety bound".to_owned());
    }
    Ok(FrameUsage {
        dimensions,
        pixels,
        base64_bytes,
    })
}

fn decode_frame(
    screenshot: &Screenshot,
    expected_dimensions: (u32, u32),
) -> Result<RgbaImage, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&screenshot.base64_data)
        .map_err(|_| "stored screenshot base64 is invalid".to_owned())?;
    if bytes.len() > MAX_RECORDING_FRAME_PNG_BYTES {
        return Err("decoded screenshot PNG exceeds the safety bound".to_owned());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_RECORDING_FRAME_DIMENSION);
    limits.max_image_height = Some(MAX_RECORDING_FRAME_DIMENSION);
    limits.max_alloc = Some(MAX_RECORDING_FRAME_PIXELS.saturating_mul(4));
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "stored screenshot PNG is invalid".to_owned())?
        .into_rgba8();
    if image.dimensions() != expected_dimensions
        || image.dimensions() != (screenshot.width, screenshot.height)
    {
        return Err("stored screenshot dimensions do not match their metadata".to_owned());
    }
    Ok(image)
}

fn inspect_frames(frames: &[Screenshot]) -> Result<((u32, u32), u64, u64), String> {
    let mut expected_dimensions = None;
    let mut base64_bytes = 0_u64;
    let mut decoded_pixels = 0_u64;
    for screenshot in frames {
        let usage = validate_frame(screenshot, expected_dimensions)?;
        expected_dimensions.get_or_insert(usage.dimensions);
        base64_bytes = checked_total(
            base64_bytes,
            usage.base64_bytes,
            MAX_RECORDING_BASE64_BYTES,
            "stored base64 data",
        )?;
        decoded_pixels = checked_total(
            decoded_pixels,
            usage.pixels,
            MAX_RECORDING_DECODED_PIXELS,
            "decoded pixels",
        )?;
    }
    let dimensions = expected_dimensions
        .ok_or_else(|| "no frames were captured for this recording".to_owned())?;
    Ok((dimensions, base64_bytes, decoded_pixels))
}

fn padded_video_dimensions(dimensions: (u32, u32)) -> Result<(u32, u32), String> {
    let width = dimensions
        .0
        .checked_add(dimensions.0 % 2)
        .ok_or_else(|| "video width overflowed".to_owned())?;
    let height = dimensions
        .1
        .checked_add(dimensions.1 % 2)
        .ok_or_else(|| "video height overflowed".to_owned())?;
    // H.264's 4:2:0 input needs even dimensions and OpenH264 rejects dimensions below 16.
    // Padding is internal to the MP4; the captured screenshot dimensions remain unchanged.
    let width = width.max(16);
    let height = height.max(16);
    if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
        return Err("video dimensions exceed the MP4 H.264 limit".to_owned());
    }
    Ok((width, height))
}

fn encode_h264_mp4(
    output: &mut fs::File,
    frames: &[Screenshot],
    screenshot_dimensions: (u32, u32),
    video_dimensions: (u32, u32),
    frame_delay_ms: u32,
) -> Result<u64, String> {
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Off)
        .adaptive_quantization(false)
        .background_detection(false)
        .skip_frames(false);
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config)
        .map_err(|error| format!("could not initialize H.264 video encoder: {error}"))?;

    let first = rgba_for_video(&frames[0], screenshot_dimensions, video_dimensions)?;
    let first_sample = encode_h264_frame(&mut encoder, &first, video_dimensions)?;
    let avc = avc_config(&first_sample, video_dimensions)?;

    let writer = BoundedSeekWriter::new(output, MAX_RECORDING_OUTPUT_BYTES)
        .map_err(|error| format!("could not initialize MP4 output: {error}"))?;
    let config = Mp4Config {
        major_brand: FourCC::from(*b"isom"),
        minor_version: 512,
        compatible_brands: vec![
            FourCC::from(*b"isom"),
            FourCC::from(*b"iso2"),
            FourCC::from(*b"avc1"),
            FourCC::from(*b"mp41"),
        ],
        timescale: 1_000,
    };
    let mut writer = Mp4Writer::write_start(writer, &config)
        .map_err(|error| format!("could not initialize MP4 video container: {error}"))?;
    writer
        .add_track(&TrackConfig::from(avc))
        .map_err(|error| format!("could not add H.264 video track: {error}"))?;
    write_h264_sample(&mut writer, first_sample, 0, frame_delay_ms)?;

    for (index, screenshot) in frames.iter().enumerate().skip(1) {
        let rgba = rgba_for_video(screenshot, screenshot_dimensions, video_dimensions)?;
        let sample = encode_h264_frame(&mut encoder, &rgba, video_dimensions)?;
        let start_time = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(u64::from(frame_delay_ms)))
            .ok_or_else(|| "video timestamp overflowed".to_owned())?;
        write_h264_sample(&mut writer, sample, start_time, frame_delay_ms)?;
    }
    writer
        .write_end()
        .map_err(|error| format!("could not finalize MP4 video container: {error}"))?;
    let mut writer = writer.into_writer();
    writer
        .flush()
        .map_err(|error| format!("could not flush MP4 video data: {error}"))?;
    Ok(writer.bytes_written())
}

fn rgba_for_video(
    screenshot: &Screenshot,
    screenshot_dimensions: (u32, u32),
    video_dimensions: (u32, u32),
) -> Result<RgbaImage, String> {
    let image = decode_frame(screenshot, screenshot_dimensions)?;
    if screenshot_dimensions == video_dimensions {
        return Ok(image);
    }
    let mut padded = RgbaImage::new(video_dimensions.0, video_dimensions.1);
    image::imageops::replace(&mut padded, &image, 0, 0);
    Ok(padded)
}

struct H264Sample {
    payload: Vec<u8>,
    nals: Vec<Vec<u8>>,
    sync: bool,
}

fn encode_h264_frame(
    encoder: &mut Encoder,
    rgba: &RgbaImage,
    dimensions: (u32, u32),
) -> Result<H264Sample, String> {
    let width = usize::try_from(dimensions.0)
        .map_err(|_| "video width does not fit the encoder".to_owned())?;
    let height = usize::try_from(dimensions.1)
        .map_err(|_| "video height does not fit the encoder".to_owned())?;
    let source = RgbaSliceU8::new(rgba.as_raw(), (width, height));
    let yuv = YUVBuffer::from_rgb_source(source);
    let stream = encoder
        .encode(&yuv)
        .map_err(|error| format!("could not encode H.264 video frame: {error}"))?;
    let sync = stream.frame_type() == FrameType::IDR;
    let nals = split_annex_b_nals(&stream.to_vec())?;
    let mut payload = Vec::new();
    for nal in &nals {
        let nal_type = nal.first().copied().unwrap_or_default() & 0x1f;
        if matches!(nal_type, 7..=9) {
            continue;
        }
        let length = u32::try_from(nal.len())
            .map_err(|_| "H.264 NAL unit exceeds the MP4 limit".to_owned())?;
        payload.extend_from_slice(&length.to_be_bytes());
        payload.extend_from_slice(nal);
    }
    if payload.is_empty() {
        return Err("H.264 encoder produced no displayable video frame".to_owned());
    }
    Ok(H264Sample {
        payload,
        nals,
        sync,
    })
}

fn avc_config(sample: &H264Sample, dimensions: (u32, u32)) -> Result<AvcConfig, String> {
    let sps = sample
        .nals
        .iter()
        .find(|nal| nal.first().is_some_and(|byte| byte & 0x1f == 7))
        .cloned()
        .ok_or_else(|| "H.264 encoder did not provide a sequence parameter set".to_owned())?;
    let pps = sample
        .nals
        .iter()
        .find(|nal| nal.first().is_some_and(|byte| byte & 0x1f == 8))
        .cloned()
        .ok_or_else(|| "H.264 encoder did not provide a picture parameter set".to_owned())?;
    Ok(AvcConfig {
        width: u16::try_from(dimensions.0)
            .map_err(|_| "video width does not fit MP4 metadata".to_owned())?,
        height: u16::try_from(dimensions.1)
            .map_err(|_| "video height does not fit MP4 metadata".to_owned())?,
        seq_param_set: sps,
        pic_param_set: pps,
    })
}

fn write_h264_sample(
    writer: &mut Mp4Writer<BoundedSeekWriter<&mut fs::File>>,
    sample: H264Sample,
    start_time: u64,
    duration: u32,
) -> Result<(), String> {
    writer
        .write_sample(
            1,
            &Mp4Sample {
                start_time,
                duration,
                rendering_offset: 0,
                is_sync: sample.sync,
                bytes: Bytes::from(sample.payload),
            },
        )
        .map_err(|error| format!("could not write H.264 MP4 frame: {error}"))
}

fn split_annex_b_nals(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let starts = data
        .windows(4)
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == [0, 0, 0, 1]).then_some(index))
        .collect::<Vec<_>>();
    if starts.is_empty() {
        return Err("H.264 encoder returned an invalid Annex B frame".to_owned());
    }
    let nals = starts
        .iter()
        .enumerate()
        .filter_map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(data.len());
            (end > start + 4).then(|| data[start + 4..end].to_vec())
        })
        .collect::<Vec<_>>();
    if nals.is_empty() {
        return Err("H.264 encoder returned no NAL units".to_owned());
    }
    Ok(nals)
}

fn checked_total(current: u64, added: u64, limit: u64, resource: &str) -> Result<u64, String> {
    let total = current
        .checked_add(added)
        .ok_or_else(|| format!("recording {resource} total overflowed"))?;
    if total > limit {
        Err(format!(
            "recording exceeds the aggregate {resource} safety bound"
        ))
    } else {
        Ok(total)
    }
}

fn validate_existing_artifact(path: &Path, overwrite: bool) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing to replace symlink recording artifact {}",
            path.display()
        )),
        Ok(metadata) if !metadata.is_file() => Err(format!(
            "refusing to replace non-regular recording artifact {}",
            path.display()
        )),
        Ok(_) if !overwrite => Err(format!(
            "recording artifact {} already exists; pass overwrite=true to replace it",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not inspect recording artifact {}: {error}",
            path.display()
        )),
    }
}

fn is_windows_device_name(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or_default();
    base.eq_ignore_ascii_case("con")
        || base.eq_ignore_ascii_case("prn")
        || base.eq_ignore_ascii_case("aux")
        || base.eq_ignore_ascii_case("nul")
        || (base.len() == 4
            && (base[..3].eq_ignore_ascii_case("com") || base[..3].eq_ignore_ascii_case("lpt"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
}

struct BoundedSeekWriter<W> {
    inner: W,
    limit: u64,
    position: u64,
    high_water_mark: u64,
}

impl<W: Seek> BoundedSeekWriter<W> {
    fn new(mut inner: W, limit: u64) -> io::Result<Self> {
        let position = inner.stream_position()?;
        Ok(Self {
            inner,
            limit,
            position,
            high_water_mark: position,
        })
    }

    const fn bytes_written(&self) -> u64 {
        self.high_water_mark
    }
}

impl<W: io::Write + Seek> io::Write for BoundedSeekWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.ensure_space(buffer.len())?;
        let written = self.inner.write(buffer)?;
        self.advance(written)?;
        Ok(written)
    }

    fn write_all(&mut self, buffer: &[u8]) -> io::Result<()> {
        self.ensure_space(buffer.len())?;
        self.inner.write_all(buffer)?;
        self.advance(buffer.len())?;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> BoundedSeekWriter<W> {
    fn ensure_space(&self, length: usize) -> io::Result<()> {
        let length = u64::try_from(length)
            .map_err(|_| io::Error::other("recording output size overflowed"))?;
        if self.position.saturating_add(length) > self.limit {
            Err(io::Error::other(
                "recording output exceeds the 32 MiB safety bound",
            ))
        } else {
            Ok(())
        }
    }

    fn advance(&mut self, length: usize) -> io::Result<()> {
        let length = u64::try_from(length)
            .map_err(|_| io::Error::other("recording output size overflowed"))?;
        self.position = self
            .position
            .checked_add(length)
            .ok_or_else(|| io::Error::other("recording output size overflowed"))?;
        self.high_water_mark = self.high_water_mark.max(self.position);
        Ok(())
    }
}

impl<W: Seek> Seek for BoundedSeekWriter<W> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = self.inner.seek(position)?;
        if position > self.limit {
            return Err(io::Error::other(
                "recording output exceeds the 32 MiB safety bound",
            ));
        }
        self.position = position;
        self.high_water_mark = self.high_water_mark.max(position);
        Ok(position)
    }
}

#[cfg(unix)]
fn secure_directory_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "could not secure recording artifact directory {}: {error}",
            path.display()
        )
    })
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "could not synchronize recording artifact directory {}: {error}",
                path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Write as _};
    use std::sync::{Arc, Mutex};

    use base64::Engine as _;
    use gpui_mcp_protocol::Screenshot;
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use mp4::{MediaType, read_mp4};
    use openh264::{decoder::Decoder, formats::YUVSource};
    use tempfile::tempdir;

    use super::{
        ArtifactStore, BoundedSeekWriter, CaptureLease, CapturePermit, DEFAULT_FRAME_DELAY_MS,
        MAX_RECORDING_BASE64_BYTES, MAX_RECORDING_FRAMES, RecordingPhase, RecordingSession,
        RecordingState, checked_total, run_encoding_job, validate_artifact_name,
        validate_frame_delay,
    };

    fn screenshot(width: u32, height: u32, color: [u8; 4]) -> Result<Screenshot, String> {
        let mut image = RgbaImage::new(width, height);
        for pixel in image.pixels_mut() {
            *pixel = Rgba(color);
        }
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .map_err(|error| error.to_string())?;
        Ok(Screenshot {
            mime_type: "image/png".to_owned(),
            base64_data: base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
            width,
            height,
        })
    }

    fn capture_one(state: &mut RecordingState, frame: Screenshot) -> Result<(), String> {
        let permit = state.begin_capture()?;
        state.complete_capture(permit, frame)?;
        Ok(())
    }

    #[test]
    fn lifecycle_rejects_duplicate_transitions_and_increments_sessions() -> Result<(), String> {
        let mut state = RecordingState::default();
        let first = state.start()?;
        assert_eq!(first, 1);
        assert!(state.start().is_err());
        assert!(state.begin_encoding().is_err());
        capture_one(&mut state, screenshot(4, 3, [1, 2, 3, 255])?)?;
        let job = state.begin_encoding()?;
        assert_eq!(job.session_id(), first);
        assert!(state.start().is_err());
        assert!(state.begin_encoding().is_err());
        assert!(state.finish_encoding(first));
        assert!(state.begin_encoding().is_err());
        assert_eq!(state.start()?, 2);
        Ok(())
    }

    #[test]
    fn completed_capture_is_bound_to_its_original_session() -> Result<(), String> {
        let mut state = RecordingState::default();
        let first = state.start()?;
        let permit = state.begin_capture()?;
        state.phase = RecordingPhase::Idle;
        let second = state.start()?;
        assert!(second > first);
        assert!(
            state
                .complete_capture(permit, screenshot(2, 2, [0, 0, 0, 255])?)
                .is_err()
        );
        let RecordingPhase::Recording(session) = &state.phase else {
            return Err("test recording session was not active".to_owned());
        };
        assert_eq!(session.session_id, second);
        assert!(session.frames.is_empty());
        Ok(())
    }

    #[test]
    fn dropped_capture_lease_releases_only_its_original_session() -> Result<(), String> {
        let state = Arc::new(Mutex::new(RecordingState::default()));
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .start()?;
        let stale_lease = CaptureLease::begin(state.clone())?;
        assert!(CaptureLease::begin(state.clone()).is_err());

        let current_permit = {
            let mut locked = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locked.phase = RecordingPhase::Idle;
            locked.start()?;
            locked.begin_capture()?
        };
        drop(stale_lease);

        let mut locked = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(locked.begin_capture().is_err());
        locked.abort_capture(current_permit);
        assert!(locked.begin_capture().is_ok());
        Ok(())
    }

    #[test]
    fn capture_preflight_rejects_capacity_before_capture() -> Result<(), String> {
        let mut state = RecordingState::default();
        let session_id = state.start()?;
        let RecordingPhase::Recording(session) = &mut state.phase else {
            return Err("test recording session was not active".to_owned());
        };
        session.frames.resize(
            MAX_RECORDING_FRAMES,
            Screenshot {
                mime_type: "image/png".to_owned(),
                base64_data: "AA==".to_owned(),
                width: 1,
                height: 1,
            },
        );
        assert!(state.begin_capture().is_err());
        assert_eq!(session_id, 1);
        Ok(())
    }

    #[test]
    fn mismatched_dimensions_are_rejected_without_losing_the_session() -> Result<(), String> {
        let mut state = RecordingState::default();
        state.start()?;
        capture_one(&mut state, screenshot(4, 3, [1, 2, 3, 255])?)?;
        let permit = state.begin_capture()?;
        assert!(
            state
                .complete_capture(permit, screenshot(5, 3, [4, 5, 6, 255])?)
                .is_err()
        );
        capture_one(&mut state, screenshot(4, 3, [7, 8, 9, 255])?)?;
        let RecordingPhase::Recording(session) = &state.phase else {
            return Err("test recording session was not active".to_owned());
        };
        assert_eq!(session.frames.len(), 2);
        Ok(())
    }

    #[test]
    fn aggregate_and_output_caps_fail_before_exceeding_the_bound() -> Result<(), String> {
        assert!(
            checked_total(
                MAX_RECORDING_BASE64_BYTES - 1,
                1,
                MAX_RECORDING_BASE64_BYTES,
                "base64"
            )
            .is_ok()
        );
        assert!(
            checked_total(
                MAX_RECORDING_BASE64_BYTES,
                1,
                MAX_RECORDING_BASE64_BYTES,
                "base64"
            )
            .is_err()
        );

        let mut writer = BoundedSeekWriter::new(Cursor::new(Vec::new()), 4)
            .map_err(|error| error.to_string())?;
        writer
            .write_all(&[1, 2, 3, 4])
            .map_err(|error| error.to_string())?;
        assert!(writer.write_all(&[5]).is_err());
        assert_eq!(writer.bytes_written(), 4);
        Ok(())
    }

    #[test]
    fn frame_delay_is_validated_instead_of_clamped() {
        assert!(validate_frame_delay(19).is_err());
        assert!(validate_frame_delay(20).is_ok());
        assert!(validate_frame_delay(DEFAULT_FRAME_DELAY_MS).is_ok());
        assert!(validate_frame_delay(10_000).is_ok());
        assert!(validate_frame_delay(10_001).is_err());
    }

    #[test]
    fn artifact_name_policy_rejects_paths_and_device_names() {
        for invalid in [
            "",
            ".mp4",
            "../recording.mp4",
            "sub/recording.mp4",
            "sub\\recording.mp4",
            "/recording.mp4",
            "recording.MP4",
            "con.mp4",
            "LPT1.test.mp4",
            " recording.mp4",
            "recording?.mp4",
        ] {
            assert!(
                validate_artifact_name(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
        for valid in ["recording.mp4", "run-01.mp4", "capture.v2_test.mp4"] {
            assert!(
                validate_artifact_name(valid).is_ok(),
                "{valid:?} should be accepted"
            );
        }
    }

    #[test]
    fn mp4_artifact_contains_a_h264_video_track() -> Result<(), String> {
        let directory = tempdir().map_err(|error| error.to_string())?;
        let store = ArtifactStore::open(directory.path().join("artifacts"))?;
        let frames = vec![
            screenshot(6, 4, [200, 40, 40, 255])?,
            screenshot(6, 4, [40, 200, 40, 255])?,
            screenshot(6, 4, [40, 40, 200, 255])?,
        ];
        let artifact = store.encode_mp4(&frames, "events.mp4", 100, false)?;
        assert_eq!(artifact.frame_count, 3);
        assert_eq!(artifact.delay_ms, 100);
        assert_eq!((artifact.width, artifact.height), (16, 16));
        assert!(artifact.bytes > 32);

        let mut video =
            read_mp4(fs::File::open(&artifact.path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        assert_eq!(video.tracks().len(), 1);
        assert_eq!(video.duration().as_millis(), 300);
        let (track_id, sequence_parameter_set, picture_parameter_set) = {
            let track = video
                .tracks()
                .values()
                .next()
                .ok_or_else(|| "MP4 did not contain a video track".to_owned())?;
            assert_eq!(
                track.media_type().map_err(|error| error.to_string())?,
                MediaType::H264
            );
            assert_eq!(track.sample_count(), 3);
            (
                track.track_id(),
                track
                    .sequence_parameter_set()
                    .map_err(|error| error.to_string())?
                    .to_vec(),
                track
                    .picture_parameter_set()
                    .map_err(|error| error.to_string())?
                    .to_vec(),
            )
        };
        let first_sample = video
            .read_sample(track_id, 1)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "MP4 did not contain the first H.264 sample".to_owned())?;
        assert!(!first_sample.bytes.is_empty());
        let mut h264_packet = Vec::new();
        append_annex_b_nal(&mut h264_packet, &sequence_parameter_set);
        append_annex_b_nal(&mut h264_packet, &picture_parameter_set);
        append_mp4_sample_as_annex_b(&mut h264_packet, &first_sample.bytes)?;
        let mut decoder = Decoder::new().map_err(|error| error.to_string())?;
        let decoded_frame = decoder
            .decode(&h264_packet)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "H.264 MP4 sample could not be decoded".to_owned())?;
        assert_eq!(decoded_frame.dimensions(), (16, 16));
        Ok(())
    }

    fn append_annex_b_nal(output: &mut Vec<u8>, nal: &[u8]) {
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(nal);
    }

    fn append_mp4_sample_as_annex_b(output: &mut Vec<u8>, sample: &[u8]) -> Result<(), String> {
        let mut remaining = sample;
        while !remaining.is_empty() {
            let Some(length_bytes) = remaining.get(..4) else {
                return Err("MP4 H.264 sample contains a truncated NAL length".to_owned());
            };
            let length = u32::from_be_bytes(
                length_bytes
                    .try_into()
                    .map_err(|_| "MP4 H.264 sample contains an invalid NAL length".to_owned())?,
            );
            let length = usize::try_from(length)
                .map_err(|_| "MP4 H.264 NAL length does not fit memory".to_owned())?;
            remaining = &remaining[4..];
            let Some((nal, tail)) = remaining.split_at_checked(length) else {
                return Err("MP4 H.264 sample contains a truncated NAL".to_owned());
            };
            append_annex_b_nal(output, nal);
            remaining = tail;
        }
        Ok(())
    }

    #[test]
    fn existing_artifact_requires_explicit_overwrite() -> Result<(), String> {
        let directory = tempdir().map_err(|error| error.to_string())?;
        let store = ArtifactStore::open(directory.path().join("artifacts"))?;
        let frames = [screenshot(2, 2, [1, 1, 1, 255])?];
        store.encode_mp4(&frames, "replace.mp4", 100, false)?;
        assert!(
            store
                .encode_mp4(&frames, "replace.mp4", 100, false)
                .is_err()
        );
        store.encode_mp4(&frames, "replace.mp4", 100, true)?;
        Ok(())
    }

    #[test]
    fn non_regular_output_failure_restores_frames_for_retry() -> Result<(), String> {
        let directory = tempdir().map_err(|error| error.to_string())?;
        let store = ArtifactStore::open(directory.path().join("artifacts"))?;
        fs::create_dir(store.directory().join("retry.mp4")).map_err(|error| error.to_string())?;
        let state = Arc::new(Mutex::new(RecordingState::default()));
        let first_session = {
            let mut locked = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let session = locked.start()?;
            capture_one(&mut locked, screenshot(3, 2, [9, 8, 7, 255])?)?;
            session
        };
        let job = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_encoding()?;
        assert!(run_encoding_job(state.clone(), &store, job, "retry.mp4", 100, false).is_err());
        fs::remove_dir(store.directory().join("retry.mp4")).map_err(|error| error.to_string())?;
        let retry = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_encoding()?;
        assert_eq!(retry.session_id(), first_session);
        let artifact = run_encoding_job(state.clone(), &store, retry, "retry.mp4", 100, false)?;
        assert_eq!(artifact.frame_count, 1);
        assert!(
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .begin_encoding()
                .is_err()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_rejects_symlink_targets() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let directory = tempdir().map_err(|error| error.to_string())?;
        let store = ArtifactStore::open(directory.path().join("artifacts"))?;
        let target = directory.path().join("outside.mp4");
        fs::write(&target, b"outside").map_err(|error| error.to_string())?;
        symlink(&target, store.directory().join("linked.mp4"))
            .map_err(|error| error.to_string())?;
        let frames = [screenshot(2, 2, [1, 2, 3, 255])?];
        assert!(store.encode_mp4(&frames, "linked.mp4", 100, true).is_err());
        assert_eq!(
            fs::read(&target).map_err(|error| error.to_string())?,
            b"outside"
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn overwrite_rejects_symlink_targets_when_windows_allows_test_symlinks() -> Result<(), String> {
        use std::os::windows::fs::symlink_file;

        let directory = tempdir().map_err(|error| error.to_string())?;
        let store = ArtifactStore::open(directory.path().join("artifacts"))?;
        let target = directory.path().join("outside.mp4");
        fs::write(&target, b"outside").map_err(|error| error.to_string())?;
        if let Err(error) = symlink_file(&target, store.directory().join("linked.mp4")) {
            if error.raw_os_error() == Some(1_314) {
                return Ok(());
            }
            return Err(error.to_string());
        }
        let frames = [screenshot(2, 2, [1, 2, 3, 255])?];
        assert!(store.encode_mp4(&frames, "linked.mp4", 100, true).is_err());
        assert_eq!(
            fs::read(&target).map_err(|error| error.to_string())?,
            b"outside"
        );
        Ok(())
    }

    #[test]
    fn encoding_job_restores_on_drop() -> Result<(), String> {
        let mut state = RecordingState::default();
        let session_id = state.start()?;
        capture_one(&mut state, screenshot(2, 2, [1, 2, 3, 255])?)?;
        let job = state.begin_encoding()?;
        assert_eq!(job.session_id(), session_id);
        state.restore_encoding(job.session);
        let RecordingPhase::Recording(RecordingSession { frames, .. }) = &state.phase else {
            return Err("recording frames were not restored".to_owned());
        };
        assert_eq!(frames.len(), 1);
        Ok(())
    }

    #[test]
    fn stale_capture_permit_does_not_match_another_session() {
        let permit = CapturePermit {
            session_id: 7,
            pointer_overlay: true,
        };
        assert_ne!(
            permit,
            CapturePermit {
                session_id: 8,
                pointer_overlay: true,
            }
        );
    }

    #[test]
    fn continuous_capture_stops_after_bounded_failures() -> Result<(), String> {
        let mut state = RecordingState::default();
        let session_id = state.start_with_config(true, 100)?;

        assert!(state.note_continuous_capture_failure(session_id, "first failure".to_owned()));
        assert!(state.note_continuous_capture_failure(session_id, "second failure".to_owned()));
        assert!(!state.note_continuous_capture_failure(session_id, "third failure".to_owned()));
        assert!(matches!(
            state.begin_encoding(),
            Err(error) if error.contains("third failure")
        ));
        Ok(())
    }
}
