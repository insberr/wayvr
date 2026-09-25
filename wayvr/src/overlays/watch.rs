use crate::{
    gui::{
        panel::{
            GuiPanel, NewGuiPanelParams, apply_custom_command, device_list::DeviceList,
            overlay_list::OverlayList, set_list::SetList,
        },
        timer::GuiTimer,
    },
    state::AppState,
    subsystem::mpris::{Mpris, MprisCmd},
    windowing::{Z_ORDER_WATCH, backend::OverlayEventData, window::OverlayWindowConfig},
};
use glam::{Affine3A, Quat, Vec3, vec3};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};
use wgui::{
    assets::AssetPathRef,
    components::{button::ComponentButton, slider::ComponentSlider},
    event::EventListenerKind,
    i18n::Translation,
    parser::{Fetchable, ParseDocumentParams},
    renderer_vk::text::custom_glyph::CustomGlyphData,
    widget::{EventResult, image::WidgetImage, label::WidgetLabel, sprite::WidgetSprite},
};
use wlx_common::{
    common::LeftRight,
    windowing::{OverlayWindowState, Positioning},
};

pub const WATCH_NAME: &str = "watch";
pub const WATCH_POS: Vec3 = vec3(-0.03, -0.01, 0.125);
pub const WATCH_ROT: Quat = Quat::from_xyzw(-0.707_106_6, 0.000_796_361_8, 0.707_106_6, 0.0);

#[derive(Default)]
struct WatchState {
    device_list: DeviceList,
    overlay_list: OverlayList,
    set_list: SetList,
    clock_12h: bool,
    mpris: Mpris,
}

pub fn create_watch(app: &mut AppState) -> anyhow::Result<OverlayWindowConfig> {
    let state = WatchState {
        clock_12h: app.session.config.clock_12h,
        ..Default::default()
    };
    let watch_xml = "gui/watch.xml";

    let mut panel =
        GuiPanel::new_from_template(app, watch_xml, state, NewGuiPanelParams::default())?;

    sets_or_overlays(&mut panel, app);

    if let Err(e) = setup_mpris(&mut panel, app) {
        log::warn!("Could not set up media controls on watch: {e:?}");
    }

    let doc_params = ParseDocumentParams {
        globals: panel.layout.state.globals.clone(),
        path: AssetPathRef::FileOrBuiltIn(watch_xml),
        extra: panel.doc_extra.take().unwrap_or_default(),
    };

    panel.on_notify = Some(Box::new({
        let name = WATCH_NAME;
        move |panel, app, event_data| {
            let mut elems_changed = panel.state.overlay_list.on_notify(
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            elems_changed |= panel.state.set_list.on_notify(
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            elems_changed |= panel.state.device_list.on_notify(
                app,
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            match event_data {
                OverlayEventData::EditModeChanged(edit_mode) => {
                    if let Ok(btn_edit_mode) = panel
                        .parser_state
                        .fetch_component_as::<ComponentButton>("btn_edit_mode")
                    {
                        btn_edit_mode.set_sticky_state(&mut panel.layout.common(), edit_mode);
                    }
                }
                OverlayEventData::SettingsChanged => {
                    panel.layout.mark_redraw();
                    sets_or_overlays(panel, app);

                    if app.session.config.clock_12h != panel.state.clock_12h {
                        panel.state.clock_12h = app.session.config.clock_12h;

                        let clock_root = panel.parser_state.get_widget_id("clock_root")?;
                        panel.layout.remove_children(clock_root);

                        panel.parser_state.instantiate_template(
                            &doc_params,
                            "Clock",
                            &mut panel.layout,
                            clock_root,
                            Default::default(),
                        )?;

                        elems_changed = true;
                    }
                }
                OverlayEventData::CustomCommand { element, command } => {
                    if let Err(e) = apply_custom_command(panel, app, &element, &command) {
                        log::warn!("Could not apply {command:?} on {name}/{element}: {e:?}");
                    } else {
                        elems_changed = true;
                    }
                }
                _ => {}
            }

            if elems_changed {
                panel.process_custom_elems(app);
            }

            Ok(())
        }
    }));

    panel
        .timers
        .push(GuiTimer::new(Duration::from_millis(100), 0));

    let positioning = Positioning::FollowHand {
        hand: LeftRight::Left,
        lerp: 1.0,
    };

    panel.update_layout(app)?;

    Ok(OverlayWindowConfig {
        name: WATCH_NAME.into(),
        z_order: Z_ORDER_WATCH,
        default_state: OverlayWindowState {
            grabbable: false,
            interactable: true,
            positioning,
            transform: Affine3A::from_scale_rotation_translation(
                Vec3::ONE * 0.115,
                WATCH_ROT,
                WATCH_POS,
            ),
            angle_fade: true,
            ..OverlayWindowState::default()
        },
        show_on_spawn: app.session.config.enable_watch,
        global: true,
        ..OverlayWindowConfig::from_backend(Box::new(panel))
    })
}

fn sets_or_overlays(panel: &mut GuiPanel<WatchState>, app: &mut AppState) {
    let visible = if app.session.config.sets_on_watch {
        [false, true]
    } else {
        [true, false]
    };

    let widget = [
        panel
            .parser_state
            .get_widget_id("panels_root")
            .unwrap_or_default(),
        panel
            .parser_state
            .get_widget_id("sets_root")
            .unwrap_or_default(),
    ];

    for i in 0..2 {
        panel
            .layout
            .alterables
            .set_widget_visible(widget[i], visible[i]);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max - 1).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

fn format_time(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[derive(Default)]
struct MprisUiCache {
    visible: Option<bool>,
    playing: Option<bool>,
    text: (String, String),
    art_url: Option<String>,
    times: (String, String),
    last_slider: f32,
    hold_until: Option<Instant>,}

fn setup_mpris(panel: &mut GuiPanel<WatchState>, app: &AppState) -> anyhow::Result<()> {
    let ps = &panel.parser_state;

    let buttons: [(&str, fn() -> MprisCmd); 3] = [
        ("mpris_prev", || MprisCmd::Previous),
        ("mpris_play", || MprisCmd::PlayPause),
        ("mpris_next", || MprisCmd::Next),
    ];
    for (id, make_cmd) in buttons {
        let tx = panel.state.mpris.sender();
        ps.fetch_component_as::<ComponentButton>(id)?
            .on_click(Rc::new(move |_, _| {
                let _ = tx.send(make_cmd());
                Ok(())
            }));
    }

    let slider = ps.fetch_component_as::<ComponentSlider>("mpris_progress")?;
    let tx = panel.state.mpris.sender();
    let id_root = ps.get_widget_id("mpris_root")?;
    let id_title = ps.get_widget_id("mpris_title")?;
    let id_artist = ps.get_widget_id("mpris_artist")?;
    let id_art = ps.get_widget_id("mpris_art")?;
    let id_play_icon = ps.get_widget_id("mpris_play_icon")?;
    let id_position = ps.get_widget_id("mpris_position")?;
    let id_duration = ps.get_widget_id("mpris_duration")?;    let id_watch_root = ps.get_widget_id("watch_root")?;

    let globals = app.wgui_globals.clone();
    let glyph = |p: &str| CustomGlyphData::from_assets(&globals, AssetPathRef::BuiltIn(p));
    let play_glyph = glyph("watch/media-play.svg")?;
    let pause_glyph = glyph("watch/media-pause.svg")?;
    let note_glyph = glyph("watch/media-note.svg")?;

    let cache = RefCell::new(MprisUiCache::default());

    panel.add_event_listener(
        id_watch_root,
        EventListenerKind::InternalStateChange,
        Box::new(move |common, _data, _app, state| {
            let info = state.mpris.snapshot();
            let mut cache = cache.borrow_mut();

            let has_player = info.player.is_some();
            if cache.visible != Some(has_player) {
                cache.visible = Some(has_player);
                common.alterables.set_widget_visible(id_root, has_player);
                common.alterables.mark_redraw();
            }
            if !has_player {
                return Ok(EventResult::Pass);
            }

            let text = (truncate(&info.title, 18), truncate(&info.artist, 24));
            if cache.text != text {
                if let Some(mut l) = common.state.widgets.get_as::<WidgetLabel>(id_title) {
                    l.set_text(common, Translation::from_raw_text(&text.0));
                }
                if let Some(mut l) = common.state.widgets.get_as::<WidgetLabel>(id_artist) {
                    l.set_text(common, Translation::from_raw_text(&text.1));
                }
                cache.text = text;
            }

            if cache.playing != Some(info.playing) {
                cache.playing = Some(info.playing);
                let g = if info.playing {
                    &pause_glyph
                } else {
                    &play_glyph
                };
                if let Some(mut s) = common.state.widgets.get_as::<WidgetSprite>(id_play_icon) {
                    s.set_content(common.alterables, Some(g.clone()));
                }
            }

            // art_url only changes after the thumbnail for it has been (re)loaded
            if cache.art_url.as_ref() != Some(&info.art_url) {
                let g = info
                    .art_png
                    .as_ref()
                    .and_then(|png| {
                        CustomGlyphData::from_bytes_raster(&globals, &info.art_url, png)
                            .inspect_err(|e| log::warn!("mpris: bad art: {e:?}"))
                            .ok()
                    })
                    .unwrap_or_else(|| note_glyph.clone());
                if let Some(mut img) = common.state.widgets.get_as::<WidgetImage>(id_art) {
                    img.set_content(common.alterables, Some(g));
                }
                cache.art_url = Some(info.art_url.clone());
            }

            // Progress/scrubbing: if the slider moved away from what we last set, the user did it.
            if !slider.is_dragging() {
                let cur = slider.get_value_primary();
                if (cur - cache.last_slider).abs() > 1e-4 {
                    let _ = tx.send(MprisCmd::SeekFraction(cur));
                    cache.last_slider = cur;
                    // don't snap back to the stale position before the player catches up
                    cache.hold_until = Some(Instant::now() + Duration::from_millis(1000));
                } else if cache.hold_until.is_none_or(|t| Instant::now() > t) {
                    slider.set_value_primary(common, info.progress().unwrap_or(0.0));
                    cache.last_slider = slider.get_value_primary();
                }
            }

            // follows the slider so scrubbing previews the target position
            let times = if info.length_us > 0 {
                let len_s = info.length_us / 1_000_000;
                let pos_s = (f64::from(slider.get_value_primary()) * len_s as f64) as i64;
                (format_time(pos_s), format_time(len_s))
            } else {
                ("-:--".into(), "-:--".into())
            };
            if cache.times != times {
                if let Some(mut l) = common.state.widgets.get_as::<WidgetLabel>(id_position) {
                    l.set_text(common, Translation::from_raw_text(&times.0));
                }
                if let Some(mut l) = common.state.widgets.get_as::<WidgetLabel>(id_duration) {
                    l.set_text(common, Translation::from_raw_text(&times.1));
                }
                cache.times = times;
            }

            Ok(EventResult::Pass)
        }),
    );

    Ok(())
}
