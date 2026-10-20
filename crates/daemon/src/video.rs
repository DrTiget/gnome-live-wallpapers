//! Video wallpapers. Preferably a GStreamer `playbin` into `gtk4paintablesink`
//! (gst-plugins-rs) wrapped in `glsinkbin`: hardware decoders (NVDEC, VA)
//! are picked by rank and frames stay GL textures up to GTK. GtkMediaFile,
//! the fallback when that sink is missing, converts and uploads every frame
//! on the CPU (measured ~75% of a core per 1080p30 monitor).

use gstreamer as gst;
use gst::prelude::*;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use std::path::Path;

pub enum Video {
    Gst { playbin: gst::Element, _bus: gst::bus::BusWatchGuard },
    Media(gtk4::MediaFile),
}

impl Video {
    pub fn new(path: &Path, volume: f64, muted: bool) -> (Video, gdk::Paintable) {
        match Self::gst(path, volume, muted) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[video] GStreamer pipeline unavailable ({e}), using GtkMediaFile");
                let media = gtk4::MediaFile::for_file(&gio::File::for_path(path));
                media.set_loop(true);
                media.set_volume(volume);
                media.set_muted(muted);
                media.play();
                let p = media.clone().upcast();
                (Video::Media(media), p)
            }
        }
    }

    fn gst(path: &Path, volume: f64, muted: bool) -> Result<(Video, gdk::Paintable), String> {
        gst::init().map_err(|e| e.to_string())?;
        let make = |name: &str| gst::ElementFactory::make(name).build().map_err(|_| format!("no {name}"));
        let sink = make("gtk4paintablesink")?;
        let paintable = sink.property::<gdk::Paintable>("paintable");
        let video_sink = if paintable.property::<Option<gdk::GLContext>>("gl-context").is_some() {
            let bin = make("glsinkbin")?;
            bin.set_property("sink", &sink);
            bin
        } else {
            sink
        };
        let uri = glib::filename_to_uri(path, None).map_err(|e| e.to_string())?;
        let playbin = gst::ElementFactory::make("playbin")
            .property("uri", uri.as_str())
            .property("video-sink", &video_sink)
            .property("volume", volume)
            .property("mute", muted)
            .build()
            .map_err(|_| "no playbin".to_string())?;
        let bus = playbin.bus().ok_or("no bus")?;
        let weak = playbin.downgrade();
        let watch = bus
            .add_watch_local(move |_, msg| {
                let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
                match msg.view() {
                    // Loop.
                    gst::MessageView::Eos(_) => {
                        let _ = p.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, gst::ClockTime::ZERO);
                    }
                    gst::MessageView::Error(e) => eprintln!("[video] {}: {}", e.error(), e.debug().unwrap_or_default()),
                    _ => {}
                }
                glib::ControlFlow::Continue
            })
            .map_err(|e| e.to_string())?;
        playbin.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        Ok((Video::Gst { playbin, _bus: watch }, paintable))
    }

    pub fn set_playing(&self, playing: bool) {
        match self {
            Video::Gst { playbin, .. } => {
                let _ = playbin.set_state(if playing { gst::State::Playing } else { gst::State::Paused });
            }
            Video::Media(m) => m.set_playing(playing),
        }
    }

    pub fn set_volume(&self, volume: f64, muted: bool) {
        match self {
            Video::Gst { playbin, .. } => {
                playbin.set_property("volume", volume);
                playbin.set_property("mute", muted);
            }
            Video::Media(m) => {
                m.set_volume(volume);
                m.set_muted(muted);
            }
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        if let Video::Gst { playbin, .. } = self {
            let _ = playbin.set_state(gst::State::Null);
        }
    }
}
