//! Minimal offline CLAP host: loads an instrument plugin, loads a patch,
//! sets parameters and renders a note list sample-accurately.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::{CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_param_info, clap_plugin_params};
use clap_sys::ext::preset_load::{CLAP_EXT_PRESET_LOAD, clap_plugin_preset_load};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::factory::preset_discovery::CLAP_PRESET_DISCOVERY_LOCATION_FILE;
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::stream::clap_istream;
use clap_sys::version::CLAP_VERSION;

use crate::arrange::TimedNote;
use crate::dsp::StereoClip;

const BLOCK: u32 = 256;

/// Folders where CLAP plugins are installed.
fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
        if let Some(h) = &home {
            dirs.push(h.join("Library/Audio/Plug-Ins/CLAP"));
        }
    } else if cfg!(target_os = "windows") {
        if let Some(p) = std::env::var_os("COMMONPROGRAMFILES") {
            dirs.push(PathBuf::from(p).join("CLAP"));
        }
        if let Some(p) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(p).join("Programs/Common/CLAP"));
        }
    } else {
        if let Some(h) = &home {
            dirs.push(h.join(".clap"));
        }
        dirs.push(PathBuf::from("/usr/lib/clap"));
    }
    if let Some(extra) = std::env::var_os("CLAP_PATH") {
        dirs.extend(std::env::split_paths(&extra));
    }
    dirs
}

pub fn find_plugin(name: &str) -> Result<PathBuf, String> {
    let direct = Path::new(name);
    if direct.exists() {
        return Ok(direct.to_path_buf());
    }
    let file = if name.ends_with(".clap") { name.to_string() } else { format!("{name}.clap") };
    plugin_dirs()
        .into_iter()
        .map(|d| d.join(&file))
        .find(|p| p.exists())
        .ok_or_else(|| format!("CLAP plugin '{name}' not found (looked in {})", plugin_dirs().iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")))
}

/// On macOS a CLAP plugin is a bundle; the loadable binary sits inside.
fn binary_path(bundle: &Path) -> PathBuf {
    if bundle.is_dir() {
        let stem = bundle.file_stem().unwrap_or_default();
        let candidate = bundle.join("Contents/MacOS").join(stem);
        if candidate.exists() {
            return candidate;
        }
        if let Ok(mut entries) = std::fs::read_dir(bundle.join("Contents/MacOS"))
            && let Some(Ok(entry)) = entries.next()
        {
            return entry.path();
        }
    }
    bundle.to_path_buf()
}

static CALLBACK_REQUESTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn host_get_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}
unsafe extern "C" fn host_request(_: *const clap_host) {}
unsafe extern "C" fn host_request_callback(_: *const clap_host) {
    CALLBACK_REQUESTED.store(true, Ordering::Relaxed);
}

fn new_host() -> Box<clap_host> {
    Box::new(clap_host {
        clap_version: CLAP_VERSION,
        host_data: ptr::null_mut(),
        name: c"mat (music as text)".as_ptr(),
        vendor: c"music-as-text".as_ptr(),
        url: c"".as_ptr(),
        version: c"0.1.0".as_ptr(),
        get_extension: Some(host_get_extension),
        request_restart: Some(host_request),
        request_process: Some(host_request),
        request_callback: Some(host_request_callback),
    })
}

#[repr(C)]
struct EventList {
    events: Vec<EventSlot>,
}

#[repr(C)]
#[derive(Clone, Copy)]
union EventSlot {
    note: clap_event_note,
    param: clap_event_param_value,
}

impl EventSlot {
    fn header(&self) -> *const clap_event_header {
        // Both variants start with the header.
        unsafe { &self.note.header }
    }
}

unsafe extern "C" fn events_size(list: *const clap_input_events) -> u32 {
    let list = unsafe { &*((*list).ctx as *const EventList) };
    list.events.len() as u32
}
unsafe extern "C" fn events_get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
    let list = unsafe { &*((*list).ctx as *const EventList) };
    list.events.get(index as usize).map_or(ptr::null(), |e| e.header())
}
unsafe extern "C" fn events_push(_: *const clap_output_events, _: *const clap_event_header) -> bool {
    true
}

struct ByteStream {
    data: Vec<u8>,
    pos: usize,
}

unsafe extern "C" fn stream_read(stream: *const clap_istream, buffer: *mut c_void, size: u64) -> i64 {
    let s = unsafe { &mut *((*stream).ctx as *mut ByteStream) };
    let n = (size as usize).min(s.data.len() - s.pos);
    unsafe { ptr::copy_nonoverlapping(s.data[s.pos..].as_ptr(), buffer as *mut u8, n) };
    s.pos += n;
    n as i64
}

pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

/// A loaded, initialized plugin instance.
pub struct ClapInstance {
    _lib: libloading::Library,
    entry: *const clap_plugin_entry,
    plugin: *const clap_plugin,
    _host: Box<clap_host>,
    pub name: String,
}

impl ClapInstance {
    pub fn load(plugin: &str, plugin_id: Option<&str>) -> Result<Self, String> {
        let bundle = find_plugin(plugin)?;
        let binary = binary_path(&bundle);
        let lib = unsafe { libloading::Library::new(&binary) }.map_err(|e| format!("cannot load {}: {e}", binary.display()))?;
        let entry: *const clap_plugin_entry = unsafe {
            let symbol = lib.get::<*const clap_plugin_entry>(b"clap_entry").map_err(|e| format!("{}: not a CLAP plugin ({e})", bundle.display()))?;
            *symbol
        };
        let host = new_host();
        unsafe {
            let bundle_c = CString::new(bundle.to_string_lossy().as_bytes()).unwrap();
            if !((*entry).init.unwrap())(bundle_c.as_ptr()) {
                return Err(format!("{}: plugin entry failed to initialize", bundle.display()));
            }
            let factory = ((*entry).get_factory.unwrap())(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
            if factory.is_null() {
                return Err("plugin has no plugin factory".into());
            }
            let count = ((*factory).get_plugin_count.unwrap())(factory);
            let mut chosen = None;
            for i in 0..count {
                let desc = ((*factory).get_plugin_descriptor.unwrap())(factory, i);
                let id = CStr::from_ptr((*desc).id).to_string_lossy().into_owned();
                let name = CStr::from_ptr((*desc).name).to_string_lossy().into_owned();
                if plugin_id.is_none_or(|want| want == id) {
                    chosen = Some(((*desc).id, name));
                    break;
                }
            }
            let Some((id_ptr, name)) = chosen else {
                return Err(format!("plugin id '{}' not found in {}", plugin_id.unwrap_or(""), bundle.display()));
            };
            let instance = ((*factory).create_plugin.unwrap())(factory, &*host, id_ptr);
            if instance.is_null() || !((*instance).init.unwrap())(instance) {
                return Err(format!("could not create plugin '{name}'"));
            }
            let me = Self { _lib: lib, entry, plugin: instance, _host: host, name };
            me.pump_main_thread();
            Ok(me)
        }
    }

    fn extension<T>(&self, id: &CStr) -> Option<&T> {
        unsafe {
            let ext = ((*self.plugin).get_extension.unwrap())(self.plugin, id.as_ptr()) as *const T;
            ext.as_ref()
        }
    }

    fn pump_main_thread(&self) {
        if CALLBACK_REQUESTED.swap(false, Ordering::Relaxed)
            && let Some(f) = unsafe { (*self.plugin).on_main_thread }
        {
            unsafe { f(self.plugin) };
        }
    }

    /// Loads a preset file, preferring the preset-load extension and
    /// falling back to the plugin's state (FXP chunk headers are skipped).
    pub fn load_preset(&self, path: &Path) -> Result<(), String> {
        if !path.exists() {
            return Err(format!("preset not found: {}", path.display()));
        }
        if let Some(ext) = self.extension::<clap_plugin_preset_load>(CLAP_EXT_PRESET_LOAD) {
            let location = CString::new(path.to_string_lossy().as_bytes()).unwrap();
            if unsafe { (ext.from_location.unwrap())(self.plugin, CLAP_PRESET_DISCOVERY_LOCATION_FILE, location.as_ptr(), ptr::null()) } {
                self.pump_main_thread();
                return Ok(());
            }
        }
        let state = self.extension::<clap_plugin_state>(CLAP_EXT_STATE).ok_or("plugin cannot load presets or state")?;
        let mut data = std::fs::read(path).map_err(|e| e.to_string())?;
        if data.starts_with(b"CcnK") && data.len() > 60 {
            data.drain(..60);
        }
        let mut stream_data = ByteStream { data, pos: 0 };
        let stream = clap_istream { ctx: &mut stream_data as *mut _ as *mut c_void, read: Some(stream_read) };
        if unsafe { (state.load.unwrap())(self.plugin, &stream) } {
            self.pump_main_thread();
            Ok(())
        } else {
            Err(format!("plugin rejected preset {}", path.display()))
        }
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        let Some(ext) = self.extension::<clap_plugin_params>(CLAP_EXT_PARAMS) else { return Vec::new() };
        let count = unsafe { (ext.count.unwrap())(self.plugin) };
        (0..count)
            .filter_map(|i| {
                let mut info: clap_param_info = unsafe { std::mem::zeroed() };
                unsafe { (ext.get_info.unwrap())(self.plugin, i, &mut info) }.then(|| ParamInfo {
                    id: info.id,
                    name: unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_string_lossy().into_owned(),
                    module: unsafe { CStr::from_ptr(info.module.as_ptr()) }.to_string_lossy().into_owned(),
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                })
            })
            .collect()
    }

    fn port_channels(&self, input: bool) -> Vec<u32> {
        let Some(ext) = self.extension::<clap_plugin_audio_ports>(CLAP_EXT_AUDIO_PORTS) else {
            return if input { vec![] } else { vec![2] };
        };
        let count = unsafe { (ext.count.unwrap())(self.plugin, input) };
        (0..count)
            .map(|i| {
                let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
                unsafe { (ext.get.unwrap())(self.plugin, i, input, &mut info) };
                info.channel_count
            })
            .collect()
    }

    /// Renders notes (times in seconds) for `length` seconds. `params` are
    /// (parameter id, plain value) pairs applied before the first note.
    pub fn render(&self, notes: &[TimedNote], params: &[(u32, f64)], length: f64, sample_rate: f32) -> Result<StereoClip, String> {
        let sr = sample_rate as f64;
        let p = self.plugin;
        let out_ports = self.port_channels(false);
        let in_ports = self.port_channels(true);
        if out_ports.is_empty() {
            return Err("plugin has no audio outputs".into());
        }
        unsafe {
            if !((*p).activate.unwrap())(p, sr, 1, BLOCK) {
                return Err("plugin failed to activate".into());
            }
            ((*p).start_processing.unwrap())(p);
        }

        // (sample, is_on, key, velocity), note-offs first on equal times.
        let mut events: Vec<(i64, bool, i16, f64)> = Vec::new();
        for n in notes {
            let on = (n.start * sr).round() as i64;
            let off = ((n.start + n.duration) * sr).round() as i64;
            let key = n.midi.round().clamp(0.0, 127.0) as i16;
            events.push((on, true, key, n.velocity.clamp(0.0, 1.0) as f64));
            events.push((off.max(on + 1), false, key, 0.0));
        }
        events.sort_by_key(|e| (e.0, e.1));

        let make_buffers = |ports: &[u32]| -> Vec<Vec<Vec<f32>>> { ports.iter().map(|&c| vec![vec![0.0f32; BLOCK as usize]; c as usize]).collect() };
        let mut out_data = make_buffers(&out_ports);
        let mut in_data = make_buffers(&in_ports);

        // A short pre-roll lets the plugin apply the patch and parameters.
        let preroll = (0.25 * sr) as i64;
        let total = preroll + (length * sr) as i64;
        let mut left = Vec::with_capacity((length * sr) as usize);
        let mut right = Vec::with_capacity((length * sr) as usize);
        let mut next = 0usize;
        let mut position = 0i64;
        let mut params_sent = false;
        let mut steady = 0i64;
        while position < total {
            let frames = (total - position).min(BLOCK as i64) as u32;
            let mut list = EventList { events: Vec::new() };
            // Wait until the plugin has applied the patch, then set parameters.
            if !params_sent && position >= preroll / 2 {
                for &(id, value) in params {
                    list.events.push(EventSlot {
                        param: clap_event_param_value {
                            header: header::<clap_event_param_value>(0, CLAP_EVENT_PARAM_VALUE),
                            param_id: id,
                            cookie: ptr::null_mut(),
                            note_id: -1,
                            port_index: -1,
                            channel: -1,
                            key: -1,
                            value,
                        },
                    });
                }
                params_sent = true;
            }
            let song_pos = position - preroll;
            while next < events.len() && events[next].0 < song_pos + frames as i64 {
                let (sample, on, key, velocity) = events[next];
                let time = (sample - song_pos).max(0) as u32;
                list.events.push(EventSlot {
                    note: clap_event_note {
                        header: header::<clap_event_note>(time, if on { CLAP_EVENT_NOTE_ON } else { CLAP_EVENT_NOTE_OFF }),
                        note_id: -1,
                        port_index: 0,
                        channel: 0,
                        key,
                        velocity,
                    },
                });
                next += 1;
            }
            let input_events = clap_input_events { ctx: &list as *const _ as *mut c_void, size: Some(events_size), get: Some(events_get) };
            let output_events = clap_output_events { ctx: ptr::null_mut(), try_push: Some(events_push) };

            let mut out_ptrs: Vec<Vec<*mut f32>> = out_data.iter_mut().map(|port| port.iter_mut().map(|c| c.as_mut_ptr()).collect()).collect();
            let mut in_ptrs: Vec<Vec<*mut f32>> = in_data.iter_mut().map(|port| port.iter_mut().map(|c| c.as_mut_ptr()).collect()).collect();
            let mut outputs: Vec<clap_audio_buffer> = out_ptrs
                .iter_mut()
                .map(|ch| clap_audio_buffer { data32: ch.as_mut_ptr(), data64: ptr::null_mut(), channel_count: ch.len() as u32, latency: 0, constant_mask: 0 })
                .collect();
            let inputs: Vec<clap_audio_buffer> = in_ptrs
                .iter_mut()
                .map(|ch| clap_audio_buffer { data32: ch.as_mut_ptr(), data64: ptr::null_mut(), channel_count: ch.len() as u32, latency: 0, constant_mask: u64::MAX })
                .collect();
            let process = clap_process {
                steady_time: steady,
                frames_count: frames,
                transport: ptr::null(),
                audio_inputs: if inputs.is_empty() { ptr::null() } else { inputs.as_ptr() },
                audio_outputs: outputs.as_mut_ptr(),
                audio_inputs_count: inputs.len() as u32,
                audio_outputs_count: outputs.len() as u32,
                in_events: &input_events,
                out_events: &output_events,
            };
            let status = unsafe { ((*p).process.unwrap())(p, &process) };
            if status == CLAP_PROCESS_ERROR {
                return Err("plugin reported a processing error".into());
            }
            self.pump_main_thread();
            if position >= preroll {
                let main = &out_data[0];
                let l = &main[0][..frames as usize];
                let r = if main.len() > 1 { &main[1][..frames as usize] } else { l };
                left.extend_from_slice(l);
                right.extend_from_slice(r);
            }
            position += frames as i64;
            steady += frames as i64;
        }
        unsafe {
            ((*p).stop_processing.unwrap())(p);
            ((*p).deactivate.unwrap())(p);
        }
        Ok(StereoClip { offset: 0, left, right })
    }
}

fn header<T>(time: u32, kind: u16) -> clap_event_header {
    clap_event_header { size: std::mem::size_of::<T>() as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_: kind, flags: 0 }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        unsafe {
            ((*self.plugin).destroy.unwrap())(self.plugin);
            ((*self.entry).deinit.unwrap())();
        }
    }
}
