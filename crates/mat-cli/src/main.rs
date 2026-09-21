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
        /// Write the stems as they were before the master's saturation,
        /// compressor, clipper and limiter, which is what they used to be.
        ///
        /// Ordinarily each stem is put through the master's own gain curve,
        /// so that playing the stems together is the mix, sample for sample.
        /// That curve is one number per sample — what the master did to the
        /// mix there — so the nonlinear stages come out right as well. With
        /// this flag the stems are the mix's linear part instead: they sum to
        /// the mix before its dynamics, and a player that wants the mastered
        /// loudness has to put its own limiter on the sum.
        #[arg(long)]
        stems_pre_master: bool,
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
    /// Pack a song and every file it reads into one zip: its includes, its
    /// samples and audio, its own instruments and the library samples it uses,
    /// with the paths in its text pointing into the pack. What comes with
    /// installed software (Logic, GarageBand, Surge, plugins) is listed in the
    /// pack's README instead.
    Pack {
        song: PathBuf,
        /// The zip to write; the song's name beside it when not given.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Measure, and hear, how a mix carries to other devices: a Sonos, a car,
    /// a phone. Takes a song, or a rendered WAV, AIFF or CAF.
    ///
    /// Prints where the mix's energy is and what summing left and right costs
    /// each band, then what every device makes of it: the level of each band
    /// against the mix, what its bass protection did, what road noise covers,
    /// and — for a song — which layers sink or vanish. The devices are models,
    /// close enough to say whether a bass line survives a small speaker.
    Translate {
        /// Not needed with --list.
        #[arg(required_unless_present = "list")]
        input: Option<PathBuf>,
        /// The devices to try, comma separated: --to sonos-five,car. All of
        /// them when not given.
        #[arg(long, value_delimiter = ',', value_name = "DEVICE")]
        to: Vec<String>,
        /// The device you are listening on. What --out writes and --play
        /// plays is corrected for it: its own colour is taken out so it is
        /// not heard on top of the simulated device's, and on headphones a
        /// simulated speaker reaches both ears as it would in a room.
        #[arg(long, value_name = "DEVICE")]
        on: Option<String>,
        /// How loud the devices play: bass protection works when it is loud,
        /// and road noise covers more when it is quiet.
        #[arg(long, value_enum, default_value_t = Loudness::Normal)]
        volume: Loudness,
        /// Write original.wav and one WAV per device into this folder, all at
        /// the original's loudness, so that what is compared is the sound.
        #[arg(short, long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Play the mix in a loop and switch between the devices while it
        /// plays: type a number or a name and Enter, q to stop.
        #[arg(long)]
        play: bool,
        /// Only these bars of a song, as `mat render --bars`.
        #[arg(long, value_name = "FROM-TO", value_parser = mat_core::BarRange::parse)]
        bars: Option<mat_core::BarRange>,
        /// Also write the measurements as JSON.
        #[arg(long, value_name = "FILE")]
        json: Option<PathBuf>,
        #[arg(long, default_value_t = 48_000)]
        sample_rate: u32,
        /// List the devices.
        #[arg(long)]
        list: bool,
    },
    /// Run the language server for .song files over stdin and stdout (for editors).
    Lsp,
}

#[derive(Clone, Copy, ValueEnum)]
enum Loudness {
    Quiet,
    Normal,
    Loud,
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
        Command::Render { song, output, sample_rate, bits, bitrate, bars, r#loop, stems, stems_pre_master, cache, stream } => {
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
            let through_master = stems.is_some() && !stems_pre_master;
            let rendering = match stream {
                true => stream_song(&timeline, sample_rate, stems.is_some(), through_master, cache.clone(), &output, depth)?,
                false => render_song(&timeline, sample_rate, stems.is_some(), through_master, cache.clone()),
            };
            let layers_peak_db = rendering.layers_peak_db;
            let master_curve_key = rendering.master_curve_key;
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
                            // A stem through the master's gain curve is not
                            // the layer's alone: a change to any other layer
                            // changes the mix, and so the curve, and so this
                            // stem. The curve is part of what it is kept under.
                            let variant = format!(
                                "{}{}{}",
                                encoding.variant(),
                                if loop_seconds.is_some() { "-loop" } else { "" },
                                master_curve_key.map_or(String::new(), |k| format!("-m{k:016x}"))
                            );
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
                    // What the stems add up to. Ordinarily every one has been
                    // through the master's own gain curve — one number per
                    // sample, what the master did to the mix there — so the
                    // sum of them is the mix itself and nothing is left for a
                    // player to do to it: `sum_peak_db` is then the mix's own
                    // peak, under the limiter's ceiling, and a player that
                    // turns the sum down by the difference turns it down by
                    // nothing. With --stems-pre-master every stage a stem has
                    // been through is linear instead, their sum is the mix
                    // before the master's dynamics, and a player that wants
                    // the mastered loudness puts its own limiter on the sum.
                    "mixing": {
                        "stems_through_master": through_master,
                        "stems_sum_to": match through_master {
                            true => "the mix, sample for sample",
                            false => "the mix before saturation, compressor, clip and limiter",
                        },
                        "applied": match through_master {
                            true => mat_core::render::MASTERED_STAGES_APPLIED,
                            false => mat_core::render::LAYER_STAGES_APPLIED,
                        },
                        "skipped": match through_master {
                            true => &[] as &[&str],
                            false => mat_core::render::LAYER_STAGES_SKIPPED,
                        },
                        "sum_peak_db": if through_master { audio.peak_db() } else { layers_peak_db },
                        // The peak the sum would have had without the curve —
                        // how hard the master was working, and what
                        // `sum_peak_db` used to say.
                        "pre_master_peak_db": layers_peak_db,
                        // What the curve was: a stem written through it is a
                        // stem of this mix and of no other.
                        "master_curve_key": master_curve_key.map(|k| format!("{k:016x}")),
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
        Command::Translate { list: true, .. } => {
            for d in mat_core::translate::DEVICES {
                println!("{:<14} {}: {}", d.name, d.title, d.about);
            }
        }
        Command::Translate { input, to, on, volume, out, play, bars, json, sample_rate, .. } => {
            let input = input.expect("clap wants an input without --list");
            let volume = match volume {
                Loudness::Quiet => mat_core::translate::Volume::Quiet,
                Loudness::Normal => mat_core::translate::Volume::Normal,
                Loudness::Loud => mat_core::translate::Volume::Loud,
            };
            return translate(&input, &to, on.as_deref(), volume, out.as_deref(), play, bars, json.as_deref(), sample_rate);
        }
        Command::Lsp => {
            mat_lsp::run_stdio().map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Command::Pack { song, output } => {
            // Arranged first, so a song that would not render is said as a
            // render says it, with its lines, rather than packed.
            if load(&song)?.is_none() {
                return Ok(ExitCode::FAILURE);
            }
            let parsed = mat_core::parse_file(&song).with_context(|| format!("reading {}", song.display()))?;
            let output = output.unwrap_or_else(|| song.with_extension("zip"));
            match mat_core::pack::pack(&song, &parsed, &output) {
                Ok(packed) => {
                    println!(
                        "packed {} files, {:.1} MB, into {}; {} path(s) rewritten to point into it",
                        packed.files,
                        packed.bytes as f64 / 1_000_000.0,
                        output.display(),
                        packed.rewritten
                    );
                    if !packed.needs.is_empty() {
                        println!("needs, installed where it is played (listed in its README.txt):");
                        for need in &packed.needs {
                            println!("  - {need}");
                        }
                    }
                }
                Err(problems) => {
                    for problem in &problems {
                        eprintln!("error: {problem}");
                    }
                    eprintln!("nothing was packed");
                    return Ok(ExitCode::FAILURE);
                }
            }
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

/// Renders the song, with its layers kept apart when `split`, and prints
/// the render's summary line.
fn render_song(timeline: &Timeline, sample_rate: u32, split: bool, through_master: bool, cache: Option<PathBuf>) -> mat_core::render::Rendering {
    let started = Instant::now();
    let stems = match render_audio_unit_stems(timeline, sample_rate) {
        Ok(stems) => stems,
        Err(err) => {
            eprintln!("warning: {err:#}");
            HashMap::new()
        }
    };
    let options = mat_core::render::RenderOptions { split, cache, stems_through_master: through_master };
    let rendering = mat_core::render::render_with(timeline, sample_rate, stems, &options);
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
    through_master: bool,
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
    let options = mat_core::render::RenderOptions { split, cache, stems_through_master: through_master };
    let mut stream = mat_core::stream::Stream::start(timeline, sample_rate, stems, &options);
    let mut writer = mat_core::wav::WavStream::create(output, sample_rate, depth).with_context(|| format!("writing {}", output.display()))?;
    let status = Status::new(output, timeline);
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
    /// How long the whole render will be, in bars and in seconds. It is known
    /// before a sample of it exists, so it is there in the first status write
    /// and never changes again: an editor can lay out the whole timeline at
    /// once instead of rescaling it as the file grows. For `--bars from-to`
    /// it is the window, which is what the file will hold.
    bars_total: u64,
    seconds_total: f64,
}

impl Status {
    fn new(output: &Path, timeline: &Timeline) -> Status {
        // The song's own length: its bars at its tempo. A whole song is its
        // last sound rounded up to a bar, which is how `mat render --bars`
        // numbers the song's bars and how `bars_written` counts them, so the
        // written and the total are the same measure. It is the music's
        // length and not the file's: the audio runs a little past the last
        // bar, where a reverb or delay tail is still ringing.
        let bars_total = match &timeline.window {
            Some(window) => u64::from(window.bars[1] - window.bars[0] + 1),
            None => u64::from(mat_core::bars::song_bars(timeline)),
        };
        Status {
            path: output.with_extension("stream.json"),
            file: output.file_name().unwrap_or(output.as_os_str()).to_string_lossy().into_owned(),
            bars_total,
            seconds_total: bars_total as f64 * timeline.bar_seconds,
        }
    }

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
            // The whole render's length, right from the first write.
            "seconds_total": self.seconds_total,
            "bars_total": self.bars_total,
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

/// What has been rendered so far, and how much of it there is. The render
/// thread appends stretches; the audio callback reads what is already there.
/// Both vectors are given the whole song's length up front, so appending never
/// reallocates under the lock and the callback is never held up by one.
#[derive(Default)]
struct Playing {
    left: Vec<f32>,
    right: Vec<f32>,
    /// The render has ended; `left` and `right` are now the whole song.
    done: bool,
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
    let sample_rate = config.sample_rate;

    // The song is rendered a stretch at a time on another thread and played as
    // it arrives, so the first sound comes in a fraction of a second instead of
    // after the whole render. Everything that carries from one sample to the
    // next carries across the stretches, so this is the same audio `mat render`
    // writes, to the byte.
    let total = timeline.end.max(0.0);
    let expected = (total * sample_rate as f64) as usize + sample_rate as usize;
    let audio = Arc::new(std::sync::Mutex::new(Playing {
        left: Vec::with_capacity(expected),
        right: Vec::with_capacity(expected),
        done: false,
    }));
    let position = Arc::new(AtomicUsize::new(0));
    let rendered = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();

    println!("{}, {} — rendering as it plays (Ctrl-C to stop)", timeline.title.as_deref().unwrap_or("untitled"), format_time(total));

    std::thread::scope(|scope| -> anyhow::Result<()> {
        let renderer = {
            let (audio, rendered) = (audio.clone(), rendered.clone());
            scope.spawn(move || {
                let stems = match render_audio_unit_stems(timeline, sample_rate) {
                    Ok(stems) => stems,
                    Err(err) => {
                        eprintln!("warning: {err:#}");
                        HashMap::new()
                    }
                };
                let options = mat_core::render::RenderOptions { split: false, cache: None, stems_through_master: false };
                let mut stream = mat_core::stream::Stream::start(timeline, sample_rate, stems, &options);
                for part in stream.by_ref() {
                    let mut a = audio.lock().expect("the audio callback never panics with the lock");
                    a.left.extend_from_slice(&part.left);
                    a.right.extend_from_slice(&part.right);
                    rendered.store(a.left.len(), Ordering::Relaxed);
                }
                // A render's last act is to cut the trailing silence and fade
                // the 50 ms before it, so the finished mix replaces the tail
                // that was handed out. It is never longer than what was played.
                let rendering = stream.finish();
                let mut a = audio.lock().expect("the audio callback never panics with the lock");
                a.left.clear();
                a.left.extend_from_slice(&rendering.mix.left);
                a.right.clear();
                a.right.extend_from_slice(&rendering.mix.right);
                a.done = true;
                rendered.store(a.left.len(), Ordering::Relaxed);
                rendering
            })
        };

        let (data, pos) = (audio.clone(), position.clone());
        let stream = device.build_output_stream(
            config,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut p = pos.load(Ordering::Relaxed);
                // If the render is momentarily behind the speakers, play
                // silence rather than skip: the position does not advance, so
                // nothing of the song is lost.
                let a = data.lock().expect("the render thread never panics with the lock");
                for frame in out.chunks_mut(channels) {
                    let (l, r) = if p < a.left.len() { (a.left[p], a.right[p]) } else { (0.0, 0.0) };
                    for (c, s) in frame.iter_mut().enumerate() {
                        *s = if c % 2 == 0 { l } else { r };
                    }
                    if p < a.left.len() {
                        p += 1;
                    }
                }
                pos.store(p, Ordering::Relaxed);
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )?;
        stream.play()?;

        // One line, rewritten: where the speakers are, and how far ahead of
        // them the render is.
        let mut render_secs = None;
        loop {
            let done = audio.lock().expect("the render thread never panics with the lock").done;
            let played = position.load(Ordering::Relaxed) as f64 / sample_rate as f64;
            let ready = rendered.load(Ordering::Relaxed) as f64 / sample_rate as f64;
            if done && render_secs.is_none() {
                render_secs = Some(started.elapsed().as_secs_f64());
            }
            let tail = match render_secs {
                Some(secs) => format!("rendered in {secs:.1}s"),
                None => format!("rendering {:.0}%", 100.0 * (ready / total.max(1e-6)).min(1.0)),
            };
            print!("\r  {} / {}  ·  {tail}    ", format_time(played), format_time(total));
            use std::io::Write;
            let _ = std::io::stdout().flush();
            if done && position.load(Ordering::Relaxed) >= rendered.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        println!();

        let rendering = renderer.join().map_err(|_| anyhow::anyhow!("the render thread panicked"))?;
        for warning in &rendering.report.warnings {
            eprintln!("warning: {warning}");
        }
        for name in &rendering.report.skipped_tracks {
            eprintln!("warning: track '{name}' uses an Audio Unit instrument and was skipped");
        }
        let audio = &rendering.mix;
        println!("played {} , peak {:.1} dBFS, rms {:.1} dBFS", format_time(audio.duration()), audio.peak_db(), audio.rms_db());
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    })
}

fn find_device(name: &str) -> anyhow::Result<&'static mat_core::translate::Device> {
    mat_core::translate::device(name).with_context(|| format!("no device '{name}' (mat translate --list names them)"))
}

/// One thing to listen to: the mix, or the mix on a device.
struct Audition {
    name: String,
    audio: mat_core::Audio,
}

#[allow(clippy::too_many_arguments)]
fn translate(
    input: &Path,
    to: &[String],
    on: Option<&str>,
    volume: mat_core::translate::Volume,
    out: Option<&Path>,
    play: bool,
    bars: Option<mat_core::BarRange>,
    json: Option<&Path>,
    sample_rate: u32,
) -> anyhow::Result<ExitCode> {
    use mat_core::translate as tr;

    let monitor = on.map(find_device).transpose()?;
    let devices: Vec<&tr::Device> = match to {
        // Heard on itself a device is the mix, which is there already.
        [] => tr::DEVICES.iter().filter(|d| Some(d.name) != monitor.map(|m| m.name)).collect(),
        names => names.iter().map(|n| find_device(n)).collect::<anyhow::Result<_>>()?,
    };
    // A song played is rendered at the rate the speakers want.
    let speakers = play.then(output_device).transpose()?;
    let sample_rate = speakers.as_ref().map_or(sample_rate, |(_, config)| config.sample_rate);

    let is_song = input.extension().is_some_and(|e| e.eq_ignore_ascii_case("song"));
    let (mix, layers, title) = if is_song {
        let Some((mut timeline, _)) = load(input)? else { return Ok(ExitCode::FAILURE) };
        if let Some(range) = bars {
            timeline = mat_core::bars::cut(&timeline, range, sample_rate).map_err(anyhow::Error::msg)?;
        }
        // The layers through the master's gain curve: they sum to the mix, so
        // what a device does to one is its share of what it does to the mix.
        let rendering = render_song(&timeline, sample_rate, true, true, None);
        (rendering.mix, rendering.layers, timeline.title.clone())
    } else {
        if bars.is_some() {
            bail!("--bars is for a song: an audio file has no bars");
        }
        let file = mat_core::sampler::audio_file::AudioFile::open(input).map_err(anyhow::Error::msg)?;
        let (left, right) = file.read_stereo(0, file.frames as i64).map_err(anyhow::Error::msg)?;
        let audio = mat_core::Audio { sample_rate: file.sample_rate as u32, left, right };
        let audio = match &speakers {
            Some((_, config)) => tr::resample(&audio, config.sample_rate),
            None => audio,
        };
        (audio, Vec::new(), None)
    };
    let title = title.unwrap_or_else(|| input.file_stem().unwrap_or_default().to_string_lossy().into_owned());

    let measured = tr::measure(&mix);
    let layer_loudness: Vec<f32> = layers.iter().map(|l| tr::loudness(&l.audio)).collect();
    let followed: Vec<tr::Layer> = layers.iter().zip(&layer_loudness).map(|(l, loudness)| tr::Layer { name: &l.layer, audio: &l.audio, loudness: *loudness }).collect();
    let listen = out.is_some() || play;

    // A device a thread: each is a pass over the whole mix and over every layer.
    let results: Vec<(tr::DeviceReport, Option<mat_core::Audio>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = devices
            .iter()
            .map(|device| {
                let (mix, measured, followed) = (&mix, &measured, &followed);
                scope.spawn(move || {
                    let radiated = tr::radiate(device, mix, volume);
                    let report = tr::report(device, measured, &radiated, followed, volume);
                    (report, listen.then(|| tr::audition(device, &radiated.audio, monitor, volume)))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("a device's thread does not panic")).collect()
    });

    print_translation(&title, &measured.report, &results, monitor, volume);
    if let Some(path) = json {
        let reports: Vec<&tr::DeviceReport> = results.iter().map(|(r, _)| r).collect();
        let value = serde_json::json!({ "title": title, "on": monitor.map(|m| m.name), "volume": format!("{volume:?}").to_lowercase(), "mix": measured.report, "devices": reports });
        std::fs::write(path, serde_json::to_string_pretty(&value)?).with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    if !listen {
        return Ok(ExitCode::SUCCESS);
    }

    // Everything at the mix's loudness, since the louder of two always sounds
    // the better; then all of it down together until nothing clips.
    let mut auditions = vec![(Audition { name: "original".into(), audio: mix }, 0.0f32)];
    for (report, audio) in results {
        let audio = audio.expect("auditioned when there is something to listen to");
        auditions.push((Audition { name: report.device.into(), audio }, (-report.loudness_change_lu).clamp(-12.0, 12.0)));
    }
    // A device that plays no bass turns a limited master back into peaks, and
    // would drag everything down a long way: past 6 dB it is left quieter
    // than the others instead, and said to be.
    let trim = auditions.iter().map(|(a, gain)| -1.0 - (a.audio.peak_db() + gain)).fold(0.0f32, f32::min).max(-6.0);
    let auditions: Vec<Audition> = auditions
        .into_iter()
        .map(|(mut a, gain)| {
            let short = (-1.0 - (a.audio.peak_db() + gain + trim)).min(0.0);
            if short < -0.1 {
                println!("{} is {:.1} dB under the others: matched, it would clip", a.name, -short);
            }
            let gain = mat_core::dsp::db_to_gain(gain + trim + short);
            a.audio.left.iter_mut().chain(a.audio.right.iter_mut()).for_each(|s| *s *= gain);
            a
        })
        .collect();

    if let Some(dir) = out {
        std::fs::create_dir_all(dir)?;
        let encoding = Encoding { format: Format::Wav, depth: BitDepth::Int24, bitrate: 256 };
        for a in &auditions {
            let path = dir.join(format!("{}.wav", a.name));
            write_audio(&path, &a.audio, &encoding).map_err(anyhow::Error::msg).with_context(|| format!("writing {}", path.display()))?;
        }
        println!("wrote {} files to {}, at one loudness ({:+.1} dB, so that none clips)", auditions.len(), dir.display(), trim);
    }
    if let Some((device, config)) = speakers {
        play_auditions(auditions, &device, config)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn print_translation(
    title: &str,
    mix: &mat_core::translate::MixReport,
    results: &[(mat_core::translate::DeviceReport, Option<mat_core::Audio>)],
    monitor: Option<&mat_core::translate::Device>,
    volume: mat_core::translate::Volume,
) {
    println!();
    println!("{title}: {}, {:.1} LUFS, peak {:.1} dBFS", format_time(mix.seconds), mix.loudness_lufs, mix.peak_db);
    println!();
    println!("  {:<9} {:>14} {:>9} {:>6} {:>10} {:>12}", "band", "", "level", "share", "mono loss", "correlation");
    for b in &mix.bands {
        println!(
            "  {:<9} {:>14} {:>6.1} dB {:>5.0}% {:>7.1} dB {:>12.2}",
            b.name,
            format!("{:.0}-{:.0} Hz", b.from_hz, b.to_hz),
            b.level_db,
            b.share * 100.0,
            b.mono_loss_db,
            b.correlation
        );
    }
    for finding in &mix.findings {
        println!("  ! {finding}");
    }

    println!();
    println!("on each device, in dB against the mix (volume {}):", format!("{volume:?}").to_lowercase());
    print!("  {:<14} {:>8}", "", "loudness");
    for b in &mix.bands {
        print!(" {:>8}", b.name);
    }
    println!();
    for (report, _) in results {
        print!("  {:<14} {:>+8.1}", report.device, report.loudness_change_lu);
        for change in &report.band_change_db {
            print!(" {:>+8.1}", change);
        }
        println!();
    }

    if results.iter().any(|(r, _)| !r.layers.is_empty()) {
        println!();
        println!("each layer against the rest of the mix, in LU (under zero it sinks, 'gone' is gone):");
        let names: Vec<&str> = results[0].0.layers.iter().map(|l| l.layer.as_str()).collect();
        print!("  {:<14}", "");
        for name in &names {
            print!(" {:>9}", name.chars().take(9).collect::<String>());
        }
        println!();
        for (report, _) in results {
            print!("  {:<14}", report.device);
            for layer in &report.layers {
                match layer.change_lu <= -40.0 {
                    true => print!(" {:>9}", "gone"),
                    false => print!(" {:>+9.1}", layer.against_mix_lu),
                }
            }
            println!();
        }
    }

    for (report, _) in results.iter().filter(|(r, _)| !r.findings.is_empty()) {
        println!();
        println!("{} ({}):", report.title, report.device);
        for finding in &report.findings {
            println!("  ! {finding}");
        }
    }
    if let Some(limit) = monitor.and_then(mat_core::translate::monitor_limit) {
        println!();
        println!("note: {limit}");
    }
    println!();
}

fn output_device() -> anyhow::Result<(cpal::Device, cpal::StreamConfig)> {
    let device = cpal::default_host().default_output_device().context("no audio output device")?;
    let config = device.default_output_config()?;
    if config.sample_format() != cpal::SampleFormat::F32 {
        bail!("unsupported device sample format {:?}", config.sample_format());
    }
    Ok((device, config.into()))
}

/// Plays the auditions in a loop, all at one position, and switches between
/// them as their numbers or names are typed.
fn play_auditions(auditions: Vec<Audition>, device: &cpal::Device, config: cpal::StreamConfig) -> anyhow::Result<()> {
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate;
    let names: Vec<String> = auditions.iter().map(|a| a.name.clone()).collect();
    let frames = auditions.iter().map(|a| a.audio.left.len()).min().unwrap_or(0);
    if frames == 0 {
        bail!("nothing to play");
    }
    let selected = Arc::new(AtomicUsize::new(0));
    let position = Arc::new(AtomicUsize::new(0));

    let (chosen, pos) = (selected.clone(), position.clone());
    // A switch is a 10 ms crossfade, so it does not click.
    let fade = (sample_rate as usize / 100).max(1);
    let (mut now, mut before, mut fading) = (0usize, 0usize, 0usize);
    let stream = device.build_output_stream(
        config,
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut p = pos.load(Ordering::Relaxed);
            let wanted = chosen.load(Ordering::Relaxed);
            if wanted != now {
                (before, now, fading) = (now, wanted, fade);
            }
            for frame in out.chunks_mut(channels) {
                let old = fading as f32 / fade as f32;
                let (a, b) = (&auditions[now].audio, &auditions[before].audio);
                let l = a.left[p] * (1.0 - old) + b.left[p] * old;
                let r = a.right[p] * (1.0 - old) + b.right[p] * old;
                for (c, s) in frame.iter_mut().enumerate() {
                    *s = if c % 2 == 0 { l } else { r };
                }
                fading = fading.saturating_sub(1);
                p = (p + 1) % frames;
            }
            pos.store(p, Ordering::Relaxed);
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;
    stream.play()?;

    for (i, name) in names.iter().enumerate() {
        println!("  {i:>2}  {name}");
    }
    println!("playing in a loop: a number or a name and Enter to switch, q to stop");
    let mut line = String::new();
    loop {
        let at = position.load(Ordering::Relaxed) as f64 / sample_rate as f64;
        print!("[{} at {}] > ", names[selected.load(Ordering::Relaxed)], format_time(at));
        use std::io::Write;
        let _ = std::io::stdout().flush();
        line.clear();
        if std::io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let word = line.trim();
        if word == "q" {
            break;
        }
        let found = word.parse::<usize>().ok().filter(|i| *i < names.len()).or_else(|| names.iter().position(|n| n.starts_with(word) && !word.is_empty()));
        match found {
            Some(i) => selected.store(i, Ordering::Relaxed),
            None if word.is_empty() => {}
            None => println!("no '{word}'"),
        }
    }
    Ok(())
}

fn format_time(seconds: f64) -> String {
    format!("{}:{:04.1}", (seconds / 60.0) as u64, seconds % 60.0)
}
