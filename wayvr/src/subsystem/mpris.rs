//! Minimal MPRIS client for the watch media controls (private patch, hacky on purpose).
//! A background thread polls the session bus and publishes a snapshot; the UI sends commands back.

use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use dbus::{
    arg::{PropMap, RefArg},
    blocking::{Connection, Proxy, stdintf::org_freedesktop_dbus::Properties},
    strings::Path as DbusPath,
};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const IFACE: &str = "org.mpris.MediaPlayer2.Player";
const TIMEOUT: Duration = Duration::from_millis(500);
const ART_SIZE: u32 = 128;

pub enum MprisCmd {
    PlayPause,
    Next,
    Previous,
    /// seek to a fraction (0..1) of the track length
    SeekFraction(f32),
}

#[derive(Clone)]
pub struct MprisInfo {
    pub player: Option<String>,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub length_us: i64,
    pub position_us: i64,
    pub rate: f64,
    pub polled_at: Instant,
    /// url the art came from; changes whenever the track art changes
    pub art_url: String,
    /// PNG-encoded thumbnail
    pub art_png: Option<Arc<Vec<u8>>>,
}

impl Default for MprisInfo {
    fn default() -> Self {
        Self {
            player: None,
            title: String::new(),
            artist: String::new(),
            playing: false,
            length_us: 0,
            position_us: 0,
            rate: 1.0,
            polled_at: Instant::now(),
            art_url: String::new(),
            art_png: None,
        }
    }
}

impl MprisInfo {
    /// Playback position extrapolated to now, in 0..1 (None if the length is unknown)
    pub fn progress(&self) -> Option<f32> {
        if self.length_us <= 0 {
            return None;
        }
        let mut pos = self.position_us as f64;
        if self.playing {
            pos += self.polled_at.elapsed().as_micros() as f64 * self.rate;
        }
        Some((pos / self.length_us as f64).clamp(0.0, 1.0) as f32)
    }
}

pub struct Mpris {
    pub info: Arc<Mutex<MprisInfo>>,
    tx: mpsc::Sender<MprisCmd>,
}

impl Default for Mpris {
    fn default() -> Self {
        let info = Arc::new(Mutex::new(MprisInfo::default()));
        let (tx, rx) = mpsc::channel();
        let info2 = info.clone();
        std::thread::Builder::new()
            .name("mpris".into())
            .spawn(move || mpris_thread(&info2, &rx))
            .expect("spawn mpris thread");
        Self { info, tx }
    }
}

impl Mpris {
    pub fn sender(&self) -> mpsc::Sender<MprisCmd> {
        self.tx.clone()
    }
    pub fn snapshot(&self) -> MprisInfo {
        self.info.lock().unwrap().clone()
    }
}

fn mpris_thread(info: &Mutex<MprisInfo>, rx: &mpsc::Receiver<MprisCmd>) {
    let mut conn: Option<Connection> = None;
    let mut state = MprisInfo::default();
    // (track id, bus name) of the current track, needed for SetPosition
    let mut track_id: Option<String> = None;

    loop {
        let cmd = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(cmd) => Some(cmd),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };

        if conn.is_none() {
            match Connection::new_session() {
                Ok(c) => conn = Some(c),
                Err(e) => {
                    log::warn!("mpris: no session bus: {e}");
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            }
        }
        let c = conn.as_ref().unwrap();

        if let Some(cmd) = cmd
            && let Some(player) = state.player.as_deref()
        {
            let p = c.with_proxy(player, "/org/mpris/MediaPlayer2", TIMEOUT);
            if let Err(e) = run_cmd(&p, &state, track_id.as_deref(), cmd) {
                log::warn!("mpris: command failed on {player}: {e}");
            }
        }

        match poll(c, &state, &mut track_id) {
            Ok(new) => state = new,
            Err(e) => {
                log::debug!("mpris: poll failed: {e:?}");
                state = MprisInfo::default();
            }
        }
        *info.lock().unwrap() = state.clone();
    }
}

fn run_cmd(
    p: &Proxy<&Connection>,
    state: &MprisInfo,
    track_id: Option<&str>,
    cmd: MprisCmd,
) -> anyhow::Result<()> {
    match cmd {
        MprisCmd::PlayPause => p.method_call(IFACE, "PlayPause", ())?,
        MprisCmd::Next => p.method_call(IFACE, "Next", ())?,
        MprisCmd::Previous => p.method_call(IFACE, "Previous", ())?,
        MprisCmd::SeekFraction(f) => {
            let target = (f64::from(f) * state.length_us as f64) as i64;
            let set_pos = track_id
                .and_then(|t| DbusPath::new(t.to_string()).ok())
                .map(|path| p.method_call::<(), _, _, _>(IFACE, "SetPosition", (path, target)));
            // fall back to relative Seek when there's no usable track id / SetPosition fails
            if !matches!(set_pos, Some(Ok(()))) {
                let cur: i64 = p.get(IFACE, "Position").unwrap_or(state.position_us);
                p.method_call::<(), _, _, _>(IFACE, "Seek", (target - cur,))?;
            }
        }    }
    Ok(())
}

fn poll(
    c: &Connection,
    prev: &MprisInfo,
    track_id: &mut Option<String>,
) -> anyhow::Result<MprisInfo> {
    let bus = c.with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", TIMEOUT);
    let (names,): (Vec<String>,) = bus.method_call("org.freedesktop.DBus", "ListNames", ())?;
    let players: Vec<String> = names
        .into_iter()
        .filter(|n| n.starts_with(PREFIX) && !n.ends_with(".playerctld"))
        .collect();

    // prefer a playing player, then the previously selected one, then whatever is first
    let status = |name: &str| -> Option<String> {
        c.with_proxy(name, "/org/mpris/MediaPlayer2", TIMEOUT)
            .get::<String>(IFACE, "PlaybackStatus")
            .ok()
    };
    let mut statuses: Vec<(String, String)> = players
        .into_iter()
        .filter_map(|n| status(&n).map(|s| (n, s)))
        .collect();
    statuses.sort_by_key(|(n, s)| (s != "Playing", Some(n) != prev.player.as_ref()));
    let Some((player, status)) = statuses.into_iter().next() else {
        *track_id = None;
        return Ok(MprisInfo::default());
    };

    let p = c.with_proxy(player.as_str(), "/org/mpris/MediaPlayer2", TIMEOUT);
    let meta: PropMap = p.get(IFACE, "Metadata").unwrap_or_default();
    let get_str = |k: &str| meta.get(k).and_then(|v| v.0.as_str()).map(str::to_string);

    let title = get_str("xesam:title").unwrap_or_default();
    let artist = meta
        .get("xesam:artist")
        .map(|v| {
            if let Some(s) = v.0.as_str() {
                s.to_string()
            } else if let Some(it) = v.0.as_iter() {
                it.filter_map(|a| a.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                String::new()
            }
        })
        .unwrap_or_default();
    let length_us = meta
        .get("mpris:length")
        .and_then(|v| v.0.as_i64())
        .unwrap_or(0);
    *track_id = get_str("mpris:trackid");

    let art_url = get_str("mpris:artUrl").unwrap_or_default();
    let art_png = if art_url == prev.art_url {
        prev.art_png.clone()
    } else if art_url.is_empty() {
        None
    } else {
        load_art(&art_url)
            .inspect_err(|e| log::warn!("mpris: could not load art {art_url}: {e:?}"))
            .ok()
            .map(Arc::new)
    };

    Ok(MprisInfo {
        title,
        artist,
        playing: status == "Playing",
        length_us,
        position_us: p.get(IFACE, "Position").unwrap_or(0),
        rate: p.get(IFACE, "Rate").unwrap_or(1.0),
        polled_at: Instant::now(),
        art_url,
        art_png,
        player: Some(player),
    })
}

/// Loads art from file:// or http(s):// and returns a small PNG thumbnail
fn load_art(url: &str) -> anyhow::Result<Vec<u8>> {
    let bytes = if let Some(path) = url.strip_prefix("file://") {
        let path = percent_encoding::percent_decode_str(path).decode_utf8()?;
        std::fs::read(path.as_ref())?
    } else if url.starts_with("http://") || url.starts_with("https://") {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .new_agent()
            .get(url)
            .call()?
            .body_mut()
            .read_to_vec()?
    } else {
        anyhow::bail!("unsupported art url");
    };

    // center-crop to square (video thumbnails are 16:9) then shrink
    let img = image::load_from_memory(&bytes)?;
    let (w, h) = (img.width(), img.height());
    let s = w.min(h);
    let img = img
        .crop_imm((w - s) / 2, (h - s) / 2, s, s)
        .thumbnail(ART_SIZE, ART_SIZE);
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}
