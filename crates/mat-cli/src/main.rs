use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use mat_core::dsp::StereoClip;
use mat_core::model::InstrumentKind;
use mat_core::encode::{Encoding, Format, write_audio};
use mat_core::wav::BitDepth;
use mat_core::{Diagnostic, Severity, Timeline};

#[derive(Parser)]
#[command(name = "mat", version, about = "Music as text: render .song files to audio")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a song and the files it includes, and print a summary.
    Check { song: PathBuf },
    /// Render a song, with the files it includes, to a WAV, FLAC or M4A (AAC) file.
    Render {
        song: PathBuf,
        /// Output file; .wav, .flac (lossless) or .m4a (AAC, macOS) picks the
        /// format. Defaults to the song name with .wav.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 48_000)]
        sample_rate: u32,
        /// Sample depth of .wav and .flac files (FLAC takes 16 or 24).
        #[arg(long, value_enum, default_value_t = Bits::B24)]
        bits: Bits,
        /// Bit rate of .m4a files in kbit/s (constrained VBR).
        #[arg(long, default_value_t = 256)]
        bitrate: u32,
        /// Render only these bars of the song, counted from 1 and both
        /// included: --bars 33-40, or one bar: --bars 9. Much faster than the
        /// whole song, for an editor that plays the bars somebody is working
        /// on while the song renders behind it.
        ///
        /// The stretch sounds as the song does at those bars for everything
        /// that starts inside it: the same notes, the same takes, the same
        /// settings, the same ducking, and the value a sweep reached earlier.
        /// Two bars in front of it are rendered and dropped, so a note that
        /// begins just before it is heard ringing at the start. Not carried
        /// in: anything sounding when that lead-in began — a longer note, a
        /// reverb or delay tail, a tb303's filter, a scratch track's record —
        /// and an audio file is cut to the stretch. The first 5 ms fade in.
        #[arg(long, value_name = "FROM-TO", value_parser = mat_core::BarRange::parse)]
        bars: Option<mat_core::BarRange>,
        /// Make a seamless loop: the length is rounded up to whole bars and
        /// the tail (reverb, releases) is folded back into the start. With
        /// --bars, the loop is exactly the bars asked for.
        #[arg(long)]
        r#loop: bool,
        /// Also write one file per layer, in the output's format, into this
        /// folder, plus manifest.json (tempo, bar length, sections, and the
        /// song's files, so an editor knows which saves need a new render)
        /// for game engines.
        #[arg(long)]
        stems: Option<PathBuf>,
        /// Keep each layer here between renders, keyed by everything that
        /// shapes it, so rendering again after an edit renders only the layers
        /// the edit touched. For editors that render on every save.
        #[arg(long)]
        cache: Option<PathBuf>,
        /// Render the song in order of time and write it as it goes, so it can
        /// be played while the rest of it renders. The output grows a stretch
        /// at a time — the first lands in a fraction of a second — and
        /// <output>.stream.json says how much of it is readable, ending
        /// "finished": true.
        ///
        /// It is one render, not two: every effect that carries something from
        /// one sample to the next carries it across a stretch, so there are no
        /// seams, and the finished file is the file `mat render` writes, to
        /// the byte. Unlike --bars, which renders a window of the song from
        /// silence, nothing is missing from it.
        ///
        /// Goes with --stems, --cache and --bars; wants a .wav output, and
        /// does not go with --loop.
        #[arg(long)]
        stream: bool,
    },
    /// Render a song and play it on the default audio device.
    Play { song: PathBuf },
    /// Convert a MIDI file into patterns and a track (printed to stdout).
    ImportMidi {
        midi: PathBuf,
        /// Name used for the track and its patterns.
        #[arg(long, default_value = "part")]
        name: String,
        /// Tempo of the song in BPM.
        #[arg(long)]
        tempo: f64,
        /// Time in seconds where bar 1 starts in the MIDI file.
        #[arg(long, default_value_t = 0.0)]
        offset: f64,
        #[arg(long, default_value = "4/4")]
        meter: String,
        /// Bars per pattern.
        #[arg(long, default_value_t = 4)]
        bars: usize,
        /// Treat notes as General MIDI drums (grid patterns with drum names).
        #[arg(long)]
        drums: bool,
        /// Ignore note velocities (useful for audio transcriptions).
        #[arg(long)]
        ignore_velocity: bool,
        /// Extend notes up to the next note (for parts played legato).
        #[arg(long)]
        legato: bool,
        /// Treat chunks as repeats when this share of notes matches (0..1, 1 = exact).
        #[arg(long, default_value_t = 1.0)]
        similarity: f64,
    },
    /// List the parameters of a CLAP plugin.
    PluginParams {
        plugin: String,
        /// Only show parameters whose name contains this text.
        #[arg(long)]
        filter: Option<String>,
    },
    /// List the built-in presets (`instrument x preset <name>`, `master preset <name>`).
    Presets,
    /// Show what an .exs sampler instrument contains (paths accept logic: and garageband:).
    Inspect { instrument: String },
    /// Export the arranged timeline as JSON (input for external renderers).
    Export {
        song: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Run the language server for .song files over stdin and stdout (for editors).
    Lsp,
}

#[derive(Clone, Copy, ValueEnum)]
enum Bits {
    #[value(name = "16")]
    B16,
    #[value(name = "24")]
    B24,
    #[value(name = "32f")]
    F32,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Check { song } => {
            let Some((timeline, _)) = load(&song)? else { return Ok(ExitCode::FAILURE) };
            print_summary(&timeline);
            println!("ok");
        }
        Command::Render { song, output, sample_rate, bits, bitrate, bars, r#loop, stems, cache, stream } => {
            let output = output.unwrap_or_else(|| song.with_extension("wav"));
            let depth = match bits {
                Bits::B16 => BitDepth::Int16,
                Bits::B24 => BitDepth::Int24,
                Bits::F32 => BitDepth::Float32,
            };
            // Checked before rendering, which can take minutes.
            let encoding = Encoding { format: Format::from_path(&output).map_err(anyhow::Error::msg)?, depth, bitrate };
            encoding.check().map_err(anyhow::Error::msg)?;
            if stream {
                if encoding.format != Format::Wav {
                    bail!("--stream writes a .wav: a FLAC or an AAC is encoded from the whole render, so there is nothing to write until it is done");
                }
                if r#loop {
                    bail!("--stream and --loop do not go together: a loop folds the song's tail back into its start, so it is finished only at the end");
                }
            }
            let Some((mut timeline, sources)) = load(&song)? else { return Ok(ExitCode::FAILURE) };
            // Only part of the song: everything outside those bars goes before
            // a voice is synthesised, so the render costs what they cost.
            if let Some(range) = bars {
                let of = mat_core::bars::song_bars(&timeline);
                timeline = mat_core::bars::cut(&timeline, range, sample_rate).map_err(anyhow::Error::msg)?;
                let window = timeline.window.as_ref().expect("a cut timeline says which bars it is");
                println!("bars {range} of {of}, {} ({} bar{} of lead-in)", format_time(window.seconds), window.lead_in_bars, if window.lead_in_bars == 1 { "" } else { "s" });
            }
            // A stretch loops over exactly the bars asked for; a whole song
            // over its length rounded up to a bar.
            let loop_seconds = r#loop.then(|| match &timeline.window {
                Some(window) => window.seconds,
                None => (timeline.end / timeline.bar_seconds - 1e-6).ceil() * timeline.bar_seconds,
            });
            // The render drops the lead-in, so everything it is measured
            // against is counted from the first bar asked for.
            let lead_in = timeline.window.as_ref().map_or(0.0, |w| w.lead_in_seconds);
            let played = (timeline.end - lead_in).max(0.0);
            // One render, split into layers when stems are wanted: a stem is
            // the song's own take of its tracks, not a solo render of them.
            let rendering = match stream {
                true => stream_song(&timeline, sample_rate, stems.is_some(), cache.clone(), &output, depth)?,
                false => render_song(&timeline, sample_rate, stems.is_some(), cache.clone()),
            };
            let layers_peak_db = rendering.layers_peak_db;
            let mut audio = rendering.mix;
            if let Some(secs) = loop_seconds {
                mat_core::render::fold_loop(&mut audio, secs);
                println!("loop: {} bars, {}", (secs / timeline.bar_seconds).round(), format_time(secs));
            }
            if encoding.format == Format::M4a && audio.peak_db() > -1.0 {
                // AAC reconstructs peaks between samples higher than the render's.
                eprintln!("warning: peak {:.1} dBFS leaves under 1 dB for the AAC encoder; the file may clip on playback", audio.peak_db());
            }
            // A streamed render has written the mix already, a stretch at a
            // time, and what is there is what would be written here.
            if !stream {
                write_audio(&output, &audio, &encoding).map_err(anyhow::Error::msg).with_context(|| format!("writing {}", output.display()))?;
            }
            println!("wrote {}", output.display());

            if let Some(dir) = stems {
                std::fs::create_dir_all(&dir)?;
                let mut written = Vec::new();
                let layer_cache = cache.as_ref().map(|dir| mat_core::render_cache::LayerCache::open(dir.clone()));
                let (mut linked, mut from_cache) = (0, 0);
                for layer in rendering.layers {
                    // Already the mix's length: the layers are cut where the mix
                    // is, so they line up in the engine without padding.
                    let mut audio = layer.audio;
                    if let Some(secs) = loop_seconds {
                        mat_core::render::fold_loop(&mut audio, secs);
                    }
                    let file = format!("{}.{}", layer.layer.replace(['/', ' '], "_"), encoding.format.extension());
                    let target = dir.join(&file);
                    from_cache += usize::from(layer.cached);
                    // A stem this cache has already written for this layer, cut to
                    // this length and written this way, is linked rather than
                    // written again: ten stems of a four-minute song are most of a
                    // gigabyte, and an edit changes one of them.
                    let kept = match (&layer_cache, layer.key) {
                        (Some(cache), Some(key)) => {
                            let variant = format!("{}{}", encoding.variant(), if loop_seconds.is_some() { "-loop" } else { "" });
                            Some((cache, cache.stem_path(key, audio.left.len(), &variant, encoding.format.extension())))
                        }
                        _ => None,
                    };
                    match kept {
                        Some((cache, stem)) => {
                            if !stem.is_file() {
                                // Under a temporary name first: a render killed
                                // half-way must not leave a short stem under a
                                // name the next render links without looking.
                                let temporary = stem.with_extension(format!("{}.tmp", std::process::id()));
                                write_audio(&temporary, &audio, &encoding).map_err(anyhow::Error::msg)?;
                                std::fs::rename(&temporary, &stem)?;
                            } else {
                                cache.touch(&stem);
                            }
                            let _ = std::fs::remove_file(&target);
                            if std::fs::hard_link(&stem, &target).is_err() {
                                std::fs::copy(&stem, &target)?;
                            }
                            linked += 1;
                        }
                        None => write_audio(&target, &audio, &encoding).map_err(anyhow::Error::msg)?,
                    }
                    println!("  {file}: peak {:.1} dBFS{}", audio.peak_db(), if layer.cached { " (cached)" } else { "" });
                    let tracks: Vec<String> = timeline.tracks.iter().filter(|t| t.layer == layer.layer).map(|t| t.name.clone()).collect();
                    written.push(serde_json::json!({
                        "layer": layer.layer,
                        "file": file,
                        "tracks": tracks,
                        // What the layer was keyed by: a player that reads the
                        // stems again after a render can keep what it made of one
                        // whose key has not changed.
                        "key": layer.key.map(|k| format!("{k:016x}")),
                        "cached": layer.cached,
                    }));
                }
                if cache.is_some() {
                    println!("layers: {from_cache} of {} from the cache, {linked} stems linked", written.len());
                }
                // Which bars these stems are, as the whole song numbers them,
                // and whether they are all of it: a reader must be able to
                // tell a stretch from the song without knowing what was asked
                // for. Everything else here — seconds, the sections, each
                // stem — is counted from the first of those bars.
                let window = timeline.window.as_ref();
                let rendered_bars = window.map_or([1, mat_core::bars::song_bars(&timeline)], |w| w.bars);
                let manifest = serde_json::json!({
                    "title": timeline.title,
                    "tempo": timeline.tempo,
                    "meter": [timeline.meter.0, timeline.meter.1],
                    "sample_rate": sample_rate,
                    "bar_seconds": timeline.bar_seconds,
                    "bars": rendered_bars,
                    "partial": window.is_some(),
                    "lead_in_bars": window.map_or(0, |w| w.lead_in_bars),
                    "seconds": loop_seconds.unwrap_or(played),
                    "loop": loop_seconds.is_some(),
                    "sections": timeline.sections.iter().map(|s| serde_json::json!({ "name": s.name, "bars": [s.from_bar, s.to_bar], "start": s.start - lead_in, "end": s.end - lead_in })).collect::<Vec<_>>(),
                    "layers": written,
                    // What the stems add up to. Every stage a stem has been
                    // through is linear, so their sum is the mix as it was
                    // before the master's dynamics; a player that wants the
                    // mastered loudness puts its own limiter on the sum, and
                    // `master` tells it what the song's would have done.
                    "mixing": {
                        "stems_sum_to": "the mix before saturation, compressor, clip and limiter",
                        "applied": mat_core::render::LAYER_STAGES_APPLIED,
                        "skipped": mat_core::render::LAYER_STAGES_SKIPPED,
                        "sum_peak_db": layers_peak_db,
                    },
                    "master": timeline.master,
                    // Every file the song was read from, the song first: a save
                    // of any of them changes what a render would make.
                    "sources": sources,
                });
                std::fs::write(dir.join("manifest.json"), serde_json::to_string_pretty(&manifest)?)?;
                println!("wrote {}", dir.join("manifest.json").display());
            }
        }
        Command::Play { song } => {
            let Some((timeline, _)) = load(&song)? else { return Ok(ExitCode::FAILURE) };
            play(&timeline)?;
        }
        Command::Presets => {
            for p in mat_core::presets::library().all() {
                println!("{:<16} {:<8} {}", p.name, p.kind, p.description);
            }
        }
        Command::Inspect { instrument } => inspect(&instrument)?,
        Command::PluginParams { plugin, filter } => {
            let instance = mat_core::clap_host::ClapInstance::load(&plugin, None).map_err(anyhow::Error::msg)?;
            let filter = filter.map(|f| f.to_lowercase());
            for p in instance.params() {
                let full = format!("{} / {}", p.module, p.name);
                if filter.as_ref().is_none_or(|f| full.to_lowercase().contains(f)) {
                    println!("{:<48} {:>8.3} .. {:<8.3} default {:.3}", full, p.min, p.max, p.default);
                }
            }
        }
        Command::ImportMidi { midi, name, tempo, offset, meter, bars, drums, ignore_velocity, legato, similarity } => {
            let data = std::fs::read(&midi).with_context(|| format!("reading {}", midi.display()))?;
            let notes = mat_core::midi::read(&data).map_err(anyhow::Error::msg)?;
            let (num, den) = meter
                .split_once('/')
                .and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?)))
                .context("meter must look like 4/4")?;
            let options = mat_core::import::ImportOptions { name, tempo, bar_quarters: num * 4.0 / den, offset, bars_per_pattern: bars, drums, ignore_velocity, legato, similarity };
            print!("{}", mat_core::import::to_song_text(&notes, &options));
        }
        Command::Lsp => {
            mat_lsp::run_stdio().map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Command::Export { song, output } => {
            let Some((timeline, _)) = load(&song)? else { return Ok(ExitCode::FAILURE) };
            let json = serde_json::to_string_pretty(&timeline)?;
            match output {
                Some(path) => std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?,
                None => println!("{json}"),
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Parses and arranges a song and the files it includes; prints diagnostics,
/// each under the path of the file it is in. Returns `None` on errors, and
/// otherwise the timeline and the song's files, absolute, the song first.
fn load(path: &Path) -> anyhow::Result<Option<(Timeline, Vec<PathBuf>)>> {
    let parsed = mat_core::parse_file(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path.display().to_string();
    let cwd = std::env::current_dir().ok();
    let print = |diags: &[Diagnostic]| {
        for d in diags {
            let Some(source) = parsed.sources.get(d.span.file) else { continue };
            // The song as it was named; an included file as a path from here.
            let file = if d.span.file == 0 {
                name.clone()
            } else {
                cwd.as_ref().and_then(|cwd| source.path.strip_prefix(cwd).ok()).unwrap_or(&source.path).display().to_string()
            };
            eprint!("{}", d.render(&file, &source.text));
        }
    };
    let mut diags = parsed.diagnostics.clone();
    match parsed.song.as_ref().map(mat_core::arrange) {
        Some(Ok(mut timeline)) => {
            print(&diags);
            let dir = path.parent().unwrap_or(Path::new("."));
            mat_core::resolve_paths(&mut timeline, dir);
            Ok(Some((timeline, parsed.sources.iter().map(|s| s.path.clone()).collect())))
        }
        failed => {
            if let Some(Err(errors)) = failed {
                diags.extend(errors);
            }
            print(&diags);
            let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
            eprintln!("{errors} error(s) in {name}");
            Ok(None)
        }
    }
}

fn inspect(instrument: &str) -> anyhow::Result<()> {
    use mat_core::sampler::Sampler;
    let path = mat_core::arrange::resolve_load_path(instrument, Path::new("."));
    let sampler = Sampler::load(Path::new(&path)).map_err(anyhow::Error::msg)?;
    let inst = &sampler.instrument;
    println!("{} ({})", inst.name, path);
    println!("  {} zones, {} groups, {} sample files", inst.zones.len(), inst.groups.len(), inst.samples.len());
    let low = inst.zones.iter().map(|z| z.key_low).min().unwrap_or(0);
    let high = inst.zones.iter().map(|z| z.key_high).max().unwrap_or(0);
    println!("  key range {} - {}", note_name(low), note_name(high));
    let layers: std::collections::BTreeSet<(u8, u8)> = inst.zones.iter().map(|z| (z.vel_low, z.vel_high)).collect();
    println!("  {} velocity ranges", layers.len());
    for g in &inst.groups {
        let zones: Vec<&mat_core::sampler::exs::Zone> = inst.groups.iter().position(|x| std::ptr::eq(x, g)).map(|gi| inst.zones.iter().filter(|z| z.group as usize == gi).collect()).unwrap_or_default();
        let keys = zones.iter().map(|z| z.key_low).min().zip(zones.iter().map(|z| z.key_high).max());
        println!(
            "  group '{}': {:?} (control {}-{}, articulation {}), velocity {}-{}, keys {}, {} zones, exclusive {}{}",
            g.name, g.enable_by, g.control_low, g.control_high, g.articulation, g.vel_low, g.vel_high,
            keys.map_or("-".into(), |(l, h)| format!("{}-{}", note_name(l), note_name(h))), zones.len(), g.exclusive,
            if g.release_trigger { ", release trigger" } else { "" }
        );
    }
    for s in &inst.samples {
        println!("  sample {} ({} Hz, {} ch)", s.file_name, s.sample_rate, s.channels);
    }
    for w in &sampler.warnings {
        println!("  warning: {w}");
    }
    Ok(())
}

fn note_name(midi: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[midi as usize % 12], midi as i32 / 12 - 1)
}

fn print_summary(t: &Timeline) {
    if let Some(title) = &t.title {
        println!("{title}");
    }
    println!("tempo {} bpm, meter {}/{}, {} length", t.tempo, t.meter.0, t.meter.1, format_time(t.end));
    for track in &t.tracks {
        println!("  {:<12} {:<12} {:>4} notes", track.name, track.instrument_name, track.notes.len());
    }
}

fn render(timeline: &Timeline, sample_rate: u32) -> mat_core::Audio {
    render_song(timeline, sample_rate, false, None).mix
}

/// Renders the song, with its layers kept apart when `split`, and prints
/// the render's summary line.
fn render_song(timeline: &Timeline, sample_rate: u32, split: bool, cache: Option<PathBuf>) -> mat_core::render::Rendering {
    let started = Instant::now();
    let stems = match render_audio_unit_stems(timeline, sample_rate) {
        Ok(stems) => stems,
        Err(err) => {
            eprintln!("warning: {err:#}");
            HashMap::new()
        }
    };
    let rendering = mat_core::render::render_with(timeline, sample_rate, stems, &mat_core::render::RenderOptions { split, cache });
    for warning in &rendering.report.warnings {
        eprintln!("warning: {warning}");
    }
    for name in &rendering.report.skipped_tracks {
        eprintln!("warning: track '{name}' uses an Audio Unit instrument and was skipped");
    }
    let secs = started.elapsed().as_secs_f64();
    let audio = &rendering.mix;
    println!(
        "rendered {} in {:.2}s ({:.0}x realtime), peak {:.1} dBFS, rms {:.1} dBFS",
        format_time(audio.duration()),
        secs,
        audio.duration() / secs.max(1e-6),
        audio.peak_db(),
        audio.rms_db()
    );
    rendering
}

/// Renders the song in order of time, writing the WAV and its `stream.json`
/// as each stretch lands, and prints what an ordinary render prints plus how
/// long the first stretch took.
fn stream_song(
    timeline: &Timeline,
    sample_rate: u32,
    split: bool,
    cache: Option<PathBuf>,
    output: &Path,
    depth: BitDepth,
) -> anyhow::Result<mat_core::render::Rendering> {
    let started = Instant::now();
    let stems = match render_audio_unit_stems(timeline, sample_rate) {
        Ok(stems) => stems,
        Err(err) => {
            eprintln!("warning: {err:#}");
            HashMap::new()
        }
    };
    let options = mat_core::render::RenderOptions { split, cache };
    let mut stream = mat_core::stream::Stream::start(timeline, sample_rate, stems, &options);
    let mut writer = mat_core::wav::WavStream::create(output, sample_rate, depth).with_context(|| format!("writing {}", output.display()))?;
    let status = Status { path: output.with_extension("stream.json"), file: output.file_name().unwrap_or(output.as_os_str()).to_string_lossy().into_owned() };
    // An empty file, said to be empty, before the first stretch: an editor
    // that is already watching learns the render has begun.
    status.write(&writer, timeline, sample_rate, false)?;

    let mut first = None;
    for part in stream.by_ref() {
        writer.append(&part.left, &part.right).with_context(|| format!("writing {}", output.display()))?;
        status.write(&writer, timeline, sample_rate, false)?;
        if first.is_none() {
            first = Some(started.elapsed().as_secs_f64());
            println!("streaming {}: {} playable after {:.2}s", output.display(), format_time(part.frames() as f64 / sample_rate as f64), first.unwrap_or_default());
        }
    }
    let rendering = stream.finish();
    // The render keeps a little less than it gave out: the trailing silence is
    // cut and the fade before it applied. That end is written again here, and
    // the file is cut to it.
    let faded = (mat_core::render::FADE_SECONDS * sample_rate as f32) as usize;
    let changed_from = rendering.mix.left.len().saturating_sub(faded);
    writer.finish(&rendering.mix, changed_from).with_context(|| format!("writing {}", output.display()))?;
    status.write(&writer, timeline, sample_rate, true)?;

    for warning in &rendering.report.warnings {
        eprintln!("warning: {warning}");
    }
    for name in &rendering.report.skipped_tracks {
        eprintln!("warning: track '{name}' uses an Audio Unit instrument and was skipped");
    }
    let secs = started.elapsed().as_secs_f64();
    let audio = &rendering.mix;
    println!(
        "streamed {} in {:.2}s ({:.0}x realtime), first sound after {:.2}s, peak {:.1} dBFS, rms {:.1} dBFS",
        format_time(audio.duration()),
        secs,
        audio.duration() / secs.max(1e-6),
        first.unwrap_or(secs),
        audio.peak_db(),
        audio.rms_db()
    );
    println!("wrote {}", status.path.display());
    Ok(rendering)
}

/// The small JSON beside a streamed render that says how much of it can be
/// read. Written after the samples it counts, and written whole, under a
/// temporary name and renamed, so a reader never sees half of one.
struct Status {
    path: PathBuf,
    file: String,
}

impl Status {
    fn write(&self, writer: &mat_core::wav::WavStream, timeline: &Timeline, sample_rate: u32, finished: bool) -> anyhow::Result<()> {
        let seconds = writer.frames() as f64 / sample_rate as f64;
        let status = serde_json::json!({
            "file": self.file,
            "sample_rate": sample_rate,
            "channels": 2,
            "bits": writer.bytes_per_frame() * 8 / 2,
            // For a reader that goes at the bytes rather than the header.
            "data_offset": writer.data_offset(),
            "bytes_per_frame": writer.bytes_per_frame(),
            "frames_written": writer.frames(),
            "seconds_written": seconds,
            "bars_written": (seconds / timeline.bar_seconds).floor() as u64,
            "bar_seconds": timeline.bar_seconds,
            "tempo": timeline.tempo,
            "finished": finished,
        });
        let temporary = self.path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&temporary, serde_json::to_string_pretty(&status)?).with_context(|| format!("writing {}", temporary.display()))?;
        std::fs::rename(&temporary, &self.path).with_context(|| format!("writing {}", self.path.display()))?;
        Ok(())
    }
}

/// Renders Audio Unit tracks with the macOS `mat-au` host into dry stems.
fn render_audio_unit_stems(timeline: &Timeline, sample_rate: u32) -> anyhow::Result<HashMap<usize, StereoClip>> {
    if !timeline.tracks.iter().any(|t| matches!(t.instrument, InstrumentKind::AudioUnit(_))) {
        return Ok(HashMap::new());
    }
    let host = find_mat_au().context("Audio Unit tracks need the mat-au host (build it with: swift build -c release --package-path swift)")?;
    let dir = std::env::temp_dir().join(format!("mat-stems-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let json = dir.join("timeline.json");
    std::fs::write(&json, serde_json::to_vec(timeline)?)?;
    println!("rendering Audio Unit tracks with {}", host.display());
    let status = std::process::Command::new(&host)
        .arg("render")
        .arg(&json)
        .args(["--out".as_ref(), dir.as_os_str()])
        .args(["--sample-rate", &sample_rate.to_string()])
        .status()
        .with_context(|| format!("running {}", host.display()))?;
    if !status.success() {
        eprintln!("warning: some Audio Unit tracks could not be rendered");
    }
    let mut stems = HashMap::new();
    for (index, track) in timeline.tracks.iter().enumerate() {
        if !matches!(track.instrument, InstrumentKind::AudioUnit(_)) {
            continue;
        }
        let path = dir.join(format!("{index}.wav"));
        if !path.exists() {
            continue;
        }
        let mut reader = hound::WavReader::open(&path).with_context(|| format!("reading stem {}", path.display()))?;
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for (i, sample) in reader.samples::<f32>().enumerate() {
            if i % 2 == 0 { left.push(sample?) } else { right.push(sample?) }
        }
        stems.insert(index, StereoClip { offset: 0, left, right });
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(stems)
}

fn find_mat_au() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MAT_AU") {
        return Some(path.into());
    }
    let exe = std::env::current_exe().ok()?;
    let mut candidates = vec![exe.with_file_name("mat-au")];
    // Development layout: <repo>/target/<profile>/mat -> <repo>/swift/.build/release/mat-au
    if let Some(repo) = exe.parent().and_then(Path::parent).and_then(Path::parent) {
        candidates.push(repo.join("swift/.build/release/mat-au"));
        candidates.push(repo.join("swift/.build/debug/mat-au"));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|p| p.join("mat-au")));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn play(timeline: &Timeline) -> anyhow::Result<()> {
    let host = cpal::default_host();
    let device = host.default_output_device().context("no audio output device")?;
    let config = device.default_output_config()?;
    if config.sample_format() != cpal::SampleFormat::F32 {
        bail!("unsupported device sample format {:?}", config.sample_format());
    }
    let config: cpal::StreamConfig = config.into();
    let channels = config.channels as usize;
    let audio = Arc::new(render(timeline, config.sample_rate));

    let position = Arc::new(AtomicUsize::new(0));
    let (data, pos) = (audio.clone(), position.clone());
    let stream = device.build_output_stream(
        config,
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut p = pos.load(Ordering::Relaxed);
            for frame in out.chunks_mut(channels) {
                let (l, r) = if p < data.left.len() { (data.left[p], data.right[p]) } else { (0.0, 0.0) };
                for (c, s) in frame.iter_mut().enumerate() {
                    *s = if c % 2 == 0 { l } else { r };
                }
                p += 1;
            }
            pos.store(p, Ordering::Relaxed);
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;
    stream.play()?;
    println!("playing... (Ctrl-C to stop)");
    while position.load(Ordering::Relaxed) < audio.left.len() {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

fn format_time(seconds: f64) -> String {
    format!("{}:{:04.1}", (seconds / 60.0) as u64, seconds % 60.0)
}
