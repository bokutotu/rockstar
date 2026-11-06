//! Small libmpv boundary. All pointers belong to one worker thread. Once the
//! render context exists, only render-thread-safe (asynchronous) client APIs
//! are used. Neither foreign callbacks nor decoder threads call GPUI.
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use gpui_kit::RenderImage;
use libloading::Library;

use super::{LoadState, MediaKind, MediaSource, PlaybackMode, PlaybackState};

pub(super) enum Command {
    Pause(bool),
    Seek(Duration),
    Volume(f32),
}

pub(super) struct Update {
    pub load: LoadState,
    pub playback: PlaybackState,
    // Only the latest frame is retained. Slow consumers cannot build a queue
    // of decoded images (or GPU textures).
    pub frame: Option<Arc<RenderImage>>,
}

pub(super) struct Player {
    commands: async_channel::Sender<Command>,
    pub updates: async_channel::Receiver<()>,
    latest: Arc<Mutex<Update>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Player {
    pub fn spawn(kind: MediaKind, source: MediaSource) -> Result<Self> {
        let (commands, receiver) = async_channel::bounded(64);
        let (changed, updates) = async_channel::bounded(1);
        let stop = Arc::new(AtomicBool::new(false));
        let latest = Arc::new(Mutex::new(Update {
            load: LoadState::Loading,
            playback: PlaybackState::default(),
            frame: None,
        }));
        let shared = latest.clone();
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("media-player".into())
            .spawn(move || {
                if let Err(error) = run(kind, source, receiver, &stopping, &shared, &changed) {
                    shared.lock().unwrap().load = LoadState::Failed(format!("{error:#}"));
                    let _ = changed.try_send(());
                }
            })?;
        Ok(Self {
            commands,
            updates,
            latest,
            stop,
            worker: Some(worker),
        })
    }

    pub fn send(&self, command: Command) -> Result<()> {
        if matches!(command, Command::Pause(true)) {
            // Hiding the last view must pause even after a burst of seek/volume
            // events fills the queue. Discard the oldest command, never pause.
            self.commands
                .force_send(command)
                .map(|_| ())
                .context("Media player is unavailable")
        } else {
            self.commands
                .try_send(command)
                .context("Media player is unavailable")
        }
    }

    pub fn take_update(&self) -> Update {
        let mut update = self.latest.lock().unwrap();
        Update {
            load: update.load.clone(),
            playback: update.playback.clone(),
            frame: update.frame.take(),
        }
    }

    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        self.stop.store(true, Ordering::Release);
        self.worker.take()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Dropping a late startup result also cancels the worker. Joining is
        // performed outside the UI thread by the owner when possible.
        self.stop.store(true, Ordering::Release);
    }
}

#[repr(C)]
struct Event {
    id: c_int,
    error: c_int,
    userdata: u64,
    data: *mut c_void,
}
#[repr(C)]
struct Property {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}
#[repr(C)]
struct EndFile {
    reason: c_int,
    error: c_int,
}
#[repr(C)]
struct Param {
    kind: c_int,
    data: *mut c_void,
}

// The client ABI is stable; these declarations are the small subset used here.
struct Api {
    create: unsafe extern "C" fn() -> *mut c_void,
    initialize: unsafe extern "C" fn(*mut c_void) -> c_int,
    option: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(*mut c_void, u64, *const *const c_char) -> c_int,
    set: unsafe extern "C" fn(*mut c_void, u64, *const c_char, c_int, *mut c_void) -> c_int,
    observe: unsafe extern "C" fn(*mut c_void, u64, *const c_char, c_int) -> c_int,
    wait: unsafe extern "C" fn(*mut c_void, f64) -> *const Event,
    destroy: unsafe extern "C" fn(*mut c_void),
    error: unsafe extern "C" fn(c_int) -> *const c_char,
    render_create: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut Param) -> c_int,
    render_update: unsafe extern "C" fn(*mut c_void) -> u64,
    render_callback:
        unsafe extern "C" fn(*mut c_void, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void),
    render: unsafe extern "C" fn(*mut c_void, *mut Param) -> c_int,
    render_free: unsafe extern "C" fn(*mut c_void),
    _library: Library,
}

impl Api {
    fn load() -> Result<Self> {
        // Nix's wrapper/devShell supplies this library on LD_LIBRARY_PATH.
        // Do not search cwd or use the user's mpv executable/configuration.
        let library = unsafe { Library::new("libmpv.so.2") }
            .context("Cannot load libmpv.so.2; start Rockstar in its media-enabled devShell or install libmpv")?;
        unsafe {
            Ok(Self {
                create: *library.get(b"mpv_create\0")?,
                initialize: *library.get(b"mpv_initialize\0")?,
                option: *library.get(b"mpv_set_option_string\0")?,
                command: *library.get(b"mpv_command_async\0")?,
                set: *library.get(b"mpv_set_property_async\0")?,
                observe: *library.get(b"mpv_observe_property\0")?,
                wait: *library.get(b"mpv_wait_event\0")?,
                destroy: *library.get(b"mpv_terminate_destroy\0")?,
                error: *library.get(b"mpv_error_string\0")?,
                render_create: *library.get(b"mpv_render_context_create\0")?,
                render_update: *library.get(b"mpv_render_context_update\0")?,
                render_callback: *library.get(b"mpv_render_context_set_update_callback\0")?,
                render: *library.get(b"mpv_render_context_render\0")?,
                render_free: *library.get(b"mpv_render_context_free\0")?,
                _library: library,
            })
        }
    }

    fn check(&self, status: c_int) -> Result<()> {
        if status < 0 {
            let message = unsafe { CStr::from_ptr((self.error)(status)) }.to_string_lossy();
            bail!("libmpv: {message}");
        }
        Ok(())
    }
}

struct Core {
    api: Api,
    handle: *mut c_void,
    renderer: *mut c_void,
}
impl Drop for Core {
    fn drop(&mut self) {
        unsafe {
            if !self.renderer.is_null() {
                (self.api.render_free)(self.renderer);
            }
            (self.api.destroy)(self.handle);
        }
    }
}
impl Core {
    fn new(video: bool) -> Result<Self> {
        let api = Api::load()?;
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            bail!("libmpv could not create a player");
        }
        let mut core = Self {
            api,
            handle,
            renderer: ptr::null_mut(),
        };
        for (key, value) in [
            ("config", "no"),
            ("load-scripts", "no"),
            ("ytdl", "no"),
            ("terminal", "no"),
            ("input-default-bindings", "no"),
            ("input-terminal", "no"),
            ("osc", "no"),
            ("osd-level", "0"),
            ("sub-auto", "no"),
            ("audio-file-auto", "no"),
            ("autoload-files", "no"),
            ("access-references", "no"),
            ("network-timeout", "10"),
            ("demuxer-max-bytes", "32MiB"),
            ("demuxer-max-back-bytes", "8MiB"),
            ("idle", "yes"),
            ("pause", "yes"),
            ("keep-open", "yes"),
            ("volume", "100"),
            ("hwdec", "no"),
            ("vo", if video { "libmpv" } else { "null" }),
        ] {
            let (key, value) = (CString::new(key)?, CString::new(value)?);
            core.api
                .check(unsafe { (core.api.option)(handle, key.as_ptr(), value.as_ptr()) })?;
        }
        if !video {
            core.api
                .check(unsafe { (core.api.option)(handle, c"vid".as_ptr(), c"no".as_ptr()) })?;
        }
        core.api.check(unsafe { (core.api.initialize)(handle) })?;
        if video {
            let mut params = [
                Param {
                    kind: 1,
                    data: c"sw".as_ptr().cast_mut().cast(),
                },
                Param {
                    kind: 0,
                    data: ptr::null_mut(),
                },
            ];
            core.api.check(unsafe {
                (core.api.render_create)(&mut core.renderer, handle, params.as_mut_ptr())
            })?;
            unsafe extern "C" fn redraw(_: *mut c_void) {}
            unsafe { (core.api.render_callback)(core.renderer, Some(redraw), ptr::null_mut()) };
        }
        for (id, name, format) in [
            (1, c"pause", 3),
            (2, c"time-pos", 5),
            (3, c"duration", 5),
            (4, c"volume", 5),
            (5, c"seekable", 3),
            (6, c"eof-reached", 3),
            (7, c"dwidth", 4),
            (8, c"dheight", 4),
        ] {
            core.api
                .check(unsafe { (core.api.observe)(handle, id, name.as_ptr(), format) })?;
        }
        Ok(core)
    }

    fn command(&self, args: &[&str]) -> Result<()> {
        let strings = args
            .iter()
            .map(|arg| CString::new(*arg))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut args = strings.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
        args.push(ptr::null());
        self.api
            .check(unsafe { (self.api.command)(self.handle, 0, args.as_ptr()) })
    }

    fn control(&self, command: Command) -> Result<()> {
        match command {
            Command::Pause(paused) => {
                let mut paused = c_int::from(paused);
                self.api.check(unsafe {
                    (self.api.set)(
                        self.handle,
                        0,
                        c"pause".as_ptr(),
                        3,
                        (&mut paused as *mut c_int).cast(),
                    )
                })
            }
            Command::Volume(volume) => {
                let mut volume = f64::from(volume) * 100.;
                self.api.check(unsafe {
                    (self.api.set)(
                        self.handle,
                        0,
                        c"volume".as_ptr(),
                        5,
                        (&mut volume as *mut f64).cast(),
                    )
                })
            }
            Command::Seek(position) => self.command(&[
                "seek",
                &position.as_secs_f64().to_string(),
                "absolute+exact",
            ]),
        }
    }

    fn frame(
        &self,
        width: i64,
        height: i64,
        scratch: &mut Vec<u64>,
    ) -> Result<Option<Arc<RenderImage>>> {
        if self.renderer.is_null() || width <= 0 || height <= 0 {
            return Ok(None);
        }
        if unsafe { (self.api.render_update)(self.renderer) } & 1 == 0 {
            return Ok(None);
        }
        // Protect the CPU renderer against pathological dimensions. Preserve
        // aspect ratio and fit large sources to a 2048px surface.
        let scale = (2048. / width.max(height) as f64).min(1.);
        let (width, height) = (
            (width as f64 * scale).round().max(1.) as u32,
            (height as f64 * scale).round().max(1.) as u32,
        );
        let mut size = [width as c_int, height as c_int];
        let mut stride = width as usize * 4;
        let bytes = stride * height as usize;
        scratch.resize(bytes.div_ceil(8), 0);
        let mut params = [
            Param {
                kind: 17,
                data: size.as_mut_ptr().cast(),
            },
            Param {
                kind: 18,
                data: c"bgr0".as_ptr().cast_mut().cast(),
            },
            Param {
                kind: 19,
                data: (&mut stride as *mut usize).cast(),
            },
            Param {
                kind: 20,
                data: scratch.as_mut_ptr().cast(),
            },
            Param {
                kind: 0,
                data: ptr::null_mut(),
            },
        ];
        self.api
            .check(unsafe { (self.api.render)(self.renderer, params.as_mut_ptr()) })?;
        // u64 storage satisfies mpv's 4-byte minimum alignment. bgr0 leaves
        // alpha unspecified, whereas GPUI requires opaque BGRA.
        let mut pixels =
            unsafe { std::slice::from_raw_parts(scratch.as_ptr().cast::<u8>(), bytes) }.to_vec();
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        let image =
            image::RgbaImage::from_raw(width, height, pixels).expect("validated frame length");
        Ok(Some(Arc::new(RenderImage::new([image::Frame::new(image)]))))
    }
}

fn run(
    kind: MediaKind,
    source: MediaSource,
    commands: async_channel::Receiver<Command>,
    stop: &AtomicBool,
    latest: &Mutex<Update>,
    changed: &async_channel::Sender<()>,
) -> Result<()> {
    source.validate_file()?;
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let core = Core::new(kind == MediaKind::Video)?;
    // File paths are absolute and are separate arguments, never shell text or
    // mpv option strings. Only resolved file/http(s) sources reach this boundary.
    let path = match &source {
        MediaSource::File(path) => CString::new(path.as_os_str().as_encoded_bytes())?,
        MediaSource::Http(url) => CString::new(url.as_str())?,
    };
    let args = [
        c"loadfile".as_ptr(),
        path.as_ptr(),
        c"replace".as_ptr(),
        ptr::null(),
    ];
    core.api
        .check(unsafe { (core.api.command)(core.handle, 0, args.as_ptr()) })?;
    let mut playback = PlaybackState::default();
    let mut load = LoadState::Loading;
    let (mut paused, mut ended, mut width, mut height) = (true, false, 0i64, 0i64);
    let mut scratch = Vec::new();
    while !stop.load(Ordering::Acquire) {
        for command in std::iter::from_fn(|| commands.try_recv().ok()).take(64) {
            core.control(command)?;
        }
        let mut dirty = false;
        // Event memory remains valid only until the next mpv_wait_event call.
        let event = unsafe { &*(core.api.wait)(core.handle, 0.01) };
        match event.id {
            1 => break,
            4 | 5 if event.error < 0 => core.api.check(event.error)?,
            7 if !event.data.is_null() => {
                let end = unsafe { &*event.data.cast::<EndFile>() };
                if end.reason == 4 {
                    core.api.check(end.error)?;
                    bail!("Media decoding failed");
                }
                if end.reason == 5 {
                    bail!("Playlists and indirect media references are not supported");
                }
                if end.reason == 0 {
                    ended = true;
                    dirty = true;
                }
            }
            8 => {
                load = LoadState::Ready;
                dirty = true;
            }
            22 if !event.data.is_null() => {
                let property = unsafe { &*event.data.cast::<Property>() };
                if !property.data.is_null() {
                    match (event.userdata, property.format) {
                        (1, 3) => paused = unsafe { *property.data.cast::<c_int>() } != 0,
                        (2, 5) => {
                            let seconds = unsafe { *property.data.cast::<f64>() };
                            if let Ok(position) = Duration::try_from_secs_f64(seconds) {
                                playback.position = position;
                            }
                        }
                        (3, 5) => {
                            playback.duration =
                                Duration::try_from_secs_f64(unsafe { *property.data.cast::<f64>() })
                                    .ok()
                        }
                        (4, 5) => {
                            playback.volume = (unsafe { *property.data.cast::<f64>() } / 100.)
                                .clamp(0., 1.) as f32
                        }
                        (5, 3) => {
                            playback.seekable = unsafe { *property.data.cast::<c_int>() } != 0
                        }
                        (6, 3) => ended = unsafe { *property.data.cast::<c_int>() } != 0,
                        (7, 4) => width = unsafe { *property.data.cast::<i64>() },
                        (8, 4) => height = unsafe { *property.data.cast::<i64>() },
                        _ => {}
                    }
                    dirty = true;
                }
            }
            _ => {}
        }
        playback.mode = if ended {
            PlaybackMode::Ended
        } else if paused {
            PlaybackMode::Paused
        } else {
            PlaybackMode::Playing
        };
        let frame = core.frame(width, height, &mut scratch)?;
        if dirty || frame.is_some() {
            let mut update = latest.lock().unwrap();
            update.load = load.clone();
            update.playback = playback.clone();
            if frame.is_some() {
                update.frame = frame;
            }
            drop(update);
            let _ = changed.try_send(());
        }
    }
    Ok(())
}
