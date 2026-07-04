prpr_l10n::tl_file!("settings");

use super::{NextPage, OffsetPage, Page, SharedState};
use crate::{
    dir, get_data, get_data_mut,
    icons::Icons,
    popup::ChooseButton,
    save_data,
    scene::BGM_VOLUME_UPDATED,
    sync_data,
    tabs::{Tabs, TitleFn},
};
use anyhow::Result;
use bytesize::ByteSize;
use inputbox::InputBox;
use macroquad::prelude::*;
use once_cell::sync::Lazy;
use prpr::{
    core::BOLD_FONT,
    ext::{open_url, poll_future, semi_white, LocalTask, RectExt, SafeTexture, ScaleType},
    scene::{request_input, return_input, show_error, show_message, take_input},
    task::Task,
    ui::{DRectButton, Scroll, Slider, Ui, PREFER_REDUCED_MOTION},
};
use prpr_l10n::{LanguageIdentifier, LANG_IDENTS, LANG_NAMES};
use reqwest::Url;
use serde::Deserialize;
use std::{
    borrow::Cow,
    fs, io,
    net::ToSocketAddrs,
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
};

const ITEM_HEIGHT: f32 = 0.15;
const INTERACT_WIDTH: f32 = 0.26;
const STATUS_PAGE: &str = "https://status.phira.cn";
const DGHUB_AUTHOR_URL: &str = "https://github.com/pingfanH";
const DGHUB_REPO_URL: &str = "https://github.com/pingfanH/phigrab";
const DGHUB_SITE_URL: &str = "http://dghub.top/";
const DGHUB_QQ_URL: &str = "https://qm.qq.com/q/FlGLR7Mfmy";

struct NameList(String);
impl<'de> Deserialize<'de> for NameList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = Vec::<String>::deserialize(deserializer)?;
        Ok(Self(s.join(", ")))
    }
}

#[derive(Deserialize)]
struct LocalizationListRaw {
    #[serde(rename = "en-US")]
    en_us: NameList,
    #[serde(rename = "fr-FR")]
    fr_fr: NameList,
    #[serde(rename = "de-DE")]
    de_de: NameList,
    #[serde(rename = "id-ID")]
    id_id: NameList,
    #[serde(rename = "ja-JP")]
    ja_jp: NameList,
    #[serde(rename = "ko-KR")]
    ko_kr: NameList,
    #[serde(rename = "pl-PL")]
    pl_pl: NameList,
    #[serde(rename = "pt-BR")]
    pt_br: NameList,
    #[serde(rename = "ru-RU")]
    ru_ru: NameList,
    #[serde(rename = "th-TH")]
    th_th: NameList,
    #[serde(rename = "zh-TW")]
    zh_tw: NameList,
    #[serde(rename = "tr-TR")]
    tr_tr: NameList,
    #[serde(rename = "vi-VN")]
    vi_vn: NameList,
}

struct LocalizationList(String);
impl<'de> Deserialize<'de> for LocalizationList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = LocalizationListRaw::deserialize(deserializer)?;
        Ok(Self(format!(
            "\
English (en-US)\n{}\n
French (fr-FR)\n{}\n
German (de-DE)\n{}\n
Indonesian (id-ID)\n{}\n
Japanese (ja-JP)\n{}\n
Korean (ko-KR)\n{}\n
Polish (pl-PL)\n{}\n
Portuguese (pt-BR)\n{}\n
Russian (ru-RU)\n{}\n
Thai (th-TH)\n{}\n
Traditional Chinese (zh-TW)\n{}\n
Turkish (tr-TR)\n{}\n
Vietnamese (vi-VN)\n{}",
            raw.en_us.0,
            raw.fr_fr.0,
            raw.de_de.0,
            raw.id_id.0,
            raw.ja_jp.0,
            raw.ko_kr.0,
            raw.pl_pl.0,
            raw.pt_br.0,
            raw.ru_ru.0,
            raw.th_th.0,
            raw.zh_tw.0,
            raw.tr_tr.0,
            raw.vi_vn.0
        )))
    }
}

#[derive(Deserialize)]
struct StaffList {
    development: NameList,
    operations: NameList,
    documentation: NameList,
    art: NameList,
    music: NameList,
    audio: NameList,
    community: NameList,
    localization: LocalizationList,
}

static STAFF_LIST: Lazy<StaffList> = Lazy::new(|| {
    let data = include_str!("../../staff.yml");
    serde_yaml::from_str(data).unwrap()
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingListType {
    General,
    Audio,
    Chart,
    Debug,
    Dghub,
    About,
}

pub struct SettingsPage {
    list_general: GeneralList,
    list_audio: AudioList,
    list_chart: ChartList,
    list_debug: DebugList,
    list_dghub: DghubList,

    tabs: Tabs<SettingListType>,

    scroll: Scroll,
    save_time: f32,

    icon: SafeTexture,
}

impl SettingsPage {
    const SAVE_TIME: f32 = 0.5;

    pub fn new(icons: Arc<Icons>) -> Self {
        Self {
            list_general: GeneralList::new(icons.lang.clone()),
            list_audio: AudioList::new(),
            list_chart: ChartList::new(),
            list_debug: DebugList::new(),
            list_dghub: DghubList::new(icons.github.clone(), icons.browser.clone(), icons.qq.clone()),

            tabs: Tabs::new([
                (SettingListType::General, || tl!("general")),
                (SettingListType::Audio, || tl!("audio")),
                (SettingListType::Chart, || tl!("chart")),
                (SettingListType::Dghub, || tl!("dghub")),
                (SettingListType::Debug, || tl!("debug")),
                (SettingListType::About, || tl!("about")),
            ] as [(SettingListType, TitleFn); 6]),

            scroll: Scroll::new(),
            save_time: f32::INFINITY,

            icon: icons.icon.clone(),
        }
    }
}

impl Page for SettingsPage {
    fn label(&self) -> Cow<'static, str> {
        tl!("label")
    }

    fn exit(&mut self) -> Result<()> {
        BGM_VOLUME_UPDATED.store(true, Ordering::Relaxed);
        if self.save_time.is_finite() {
            save_data()?;
        }
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        let t = s.t;
        if match self.tabs.selected() {
            SettingListType::General => self.list_general.top_touch(touch, t),
            SettingListType::Audio => self.list_audio.top_touch(touch, t),
            SettingListType::Chart => self.list_chart.top_touch(touch, t),
            SettingListType::Debug => self.list_debug.top_touch(touch, t),
            SettingListType::Dghub => self.list_dghub.top_touch(touch, t),
            SettingListType::About => false,
        } {
            return Ok(true);
        }

        if self.tabs.touch(touch, s.rt) {
            return Ok(true);
        }

        if self.scroll.touch(touch, t) {
            return Ok(true);
        }
        if let Some(p) = match self.tabs.selected() {
            SettingListType::General => self.list_general.touch(touch, t)?,
            SettingListType::Audio => self.list_audio.touch(touch, t)?,
            SettingListType::Chart => self.list_chart.touch(touch, t)?,
            SettingListType::Debug => self.list_debug.touch(touch, t)?,
            SettingListType::Dghub => self.list_dghub.touch(touch, t)?,
            SettingListType::About => None,
        } {
            if p {
                self.save_time = t;
            }
            self.scroll.y_scroller.halt();
            return Ok(true);
        }
        Ok(false)
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        let changed = match self.tabs.selected() {
            SettingListType::General => self.list_general.update(t)?,
            SettingListType::Audio => self.list_audio.update(t)?,
            SettingListType::Chart => self.list_chart.update(t)?,
            SettingListType::Debug => self.list_debug.update(t)?,
            SettingListType::Dghub => self.list_dghub.update(t)?,
            SettingListType::About => false,
        };
        self.scroll.update(t);
        if changed {
            self.save_time = t;
        }
        if t > self.save_time + Self::SAVE_TIME {
            save_data()?;
            self.save_time = f32::INFINITY;
        }
        Ok(())
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        let rt = s.rt;

        s.fader.render(ui, s.t, |ui| {
            let r = ui.content_rect();
            self.tabs.render(ui, rt, r, |ui, item| {
                let r = r.feather(-0.01);
                self.scroll.size((r.w, r.h));
                ui.scope(|ui| {
                    ui.dx(r.x);
                    ui.dy(r.y);
                    self.scroll.render(ui, |ui| match item {
                        SettingListType::General => self.list_general.render(ui, r, t),
                        SettingListType::Audio => self.list_audio.render(ui, r, t),
                        SettingListType::Chart => self.list_chart.render(ui, r, t),
                        SettingListType::Debug => self.list_debug.render(ui, r, t),
                        SettingListType::Dghub => self.list_dghub.render(ui, r, t),
                        SettingListType::About => render_about(ui, r, &self.icon),
                    });
                });

                Ok(())
            })
        })?;

        Ok(())
    }

    fn next_page(&mut self) -> NextPage {
        if matches!(self.tabs.selected(), SettingListType::Audio) {
            return self.list_audio.next_page().unwrap_or_default();
        }
        NextPage::None
    }
}

fn render_about(ui: &mut Ui, mut r: Rect, icon: &SafeTexture) -> (f32, f32) {
    r.x = 0.;
    r.y = 0.;
    let ow = r.w;
    let r = r.feather(-0.02);

    let ct = r.center();
    let s = 0.1;
    let ir = Rect::new(ct.x - s, r.y + 0.05, s * 2., s * 2.);
    ui.fill_path(&ir.rounded(0.02), (**icon, ir));

    let staff = &*STAFF_LIST;
    let text = tl!(
        "about-content",
        "version" => format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")),

        "development" => &staff.development.0,
        "operations" => &staff.operations.0,
        "documentation" => &staff.documentation.0,
        "art" => &staff.art.0,
        "music" => &staff.music.0,
        "audio" => &staff.audio.0,
        "community" => &staff.community.0,
        "localization" => &staff.localization.0
    );
    let (first, text) = text.split_once('\n').unwrap();
    let tr = ui
        .text(first)
        .pos(ct.x, ir.bottom() + 0.03)
        .anchor(0.5, 0.)
        .size(0.6)
        .draw_using(&BOLD_FONT);

    let r = ui
        .text(text.trim())
        .pos(r.x, tr.bottom() + 0.06)
        .size(0.55)
        .multiline()
        .max_width(r.w)
        .h_center()
        .draw();

    (ow, r.bottom() + 0.03)
}

fn render_title<'a>(ui: &mut Ui, title: impl Into<Cow<'a, str>>, subtitle: Option<Cow<'a, str>>) -> f32 {
    const TITLE_SIZE: f32 = 0.6;
    const SUBTITLE_SIZE: f32 = 0.35;
    const LEFT: f32 = 0.06;
    const PAD: f32 = 0.01;
    const SUB_MAX_WIDTH: f32 = 1.4;
    if let Some(subtitle) = subtitle {
        let title = title.into();
        let r1 = ui.text(Cow::clone(&title)).size(TITLE_SIZE).measure();
        let r2 = ui
            .text(Cow::clone(&subtitle))
            .size(SUBTITLE_SIZE)
            .max_width(SUB_MAX_WIDTH)
            .no_baseline()
            .measure();
        let h = r1.h + PAD + r2.h;
        let r1 = ui
            .text(subtitle)
            .pos(LEFT, (ITEM_HEIGHT + h) / 2.)
            .anchor(0., 1.)
            .size(SUBTITLE_SIZE)
            .max_width(SUB_MAX_WIDTH)
            .color(semi_white(0.6))
            .draw()
            .right();
        let r2 = ui
            .text(title)
            .pos(LEFT, (ITEM_HEIGHT - h) / 2.)
            .no_baseline()
            .size(TITLE_SIZE)
            .draw()
            .right();
        r1.max(r2)
    } else {
        ui.text(title.into())
            .pos(LEFT, ITEM_HEIGHT / 2.)
            .anchor(0., 0.5)
            .no_baseline()
            .size(TITLE_SIZE)
            .draw()
            .right()
    }
}

#[inline]
fn render_switch(ui: &mut Ui, r: Rect, t: f32, btn: &mut DRectButton, on: bool) {
    btn.render_text(ui, r, t, if on { ttl!("switch-on") } else { ttl!("switch-off") }, 0.5, on);
}

fn next_dghub_channel(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "a" => "b",
        "b" => "both",
        _ => "a",
    }
    .to_owned()
}

fn dghub_channel_label(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "a" => "A",
        "b" => "B",
        _ => "双通道",
    }
}

fn next_dghub_indicator_style(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "ring" => "bar",
        "bar" => "hidden",
        "hidden" => "ring",
        _ => "bar",
    }
    .to_owned()
}

fn dghub_indicator_style_label(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "bar" => "横条",
        "hidden" => "隐藏",
        _ => "圆环",
    }
}

const DGHUB_PRESETS: &[&str] = &[
    "CS2-受伤",
    "CS2-闪光",
    "CS2-烟雾",
    "CS2-燃烧",
    "CS2-死亡",
    "呼吸",
    "潮汐",
    "连击",
    "快速按捏",
    "心跳节奏",
    "颗粒摩擦",
    "波浪涟漪",
];

fn dghub_preset_options() -> Vec<String> {
    DGHUB_PRESETS.iter().map(|it| (*it).to_owned()).collect()
}

fn dghub_preset_index(value: &str) -> usize {
    DGHUB_PRESETS.iter().position(|it| *it == value).unwrap_or(0)
}

fn dghub_preset_value(index: usize) -> String {
    DGHUB_PRESETS.get(index).unwrap_or(&DGHUB_PRESETS[0]).to_string()
}

fn dghub_preset_button(value: &str) -> ChooseButton {
    ChooseButton::new()
        .with_options(dghub_preset_options())
        .with_selected(dghub_preset_index(value))
}

fn touch_u32_slider(slider: &mut Slider, touch: &Touch, t: f32, value: &mut u32) -> Option<bool> {
    let mut current = *value as f32;
    let result = slider.touch(touch, t, &mut current);
    if matches!(result, Some(true)) {
        *value = current.round().clamp(0., 100.) as u32;
    }
    result
}

fn touch_f64_slider(slider: &mut Slider, touch: &Touch, t: f32, value: &mut f64) -> Option<bool> {
    let mut current = *value as f32;
    let result = slider.touch(touch, t, &mut current);
    if matches!(result, Some(true)) {
        *value = (current as f64).clamp(0., 3.);
    }
    result
}

#[inline]
fn right_rect(w: f32) -> Rect {
    let rh = ITEM_HEIGHT * 2. / 3.;
    Rect::new(w - 0.3, (ITEM_HEIGHT - rh) / 2., INTERACT_WIDTH, rh)
}

#[inline]
fn icon_rect(r: Rect) -> Rect {
    Rect::new(r.right() - r.h, r.y, r.h, r.h)
}

fn render_link_button(ui: &mut Ui, r: Rect, t: f32, btn: &mut DRectButton, icon: &SafeTexture) {
    btn.render_shadow(ui, r, t, |ui, path| {
        ui.fill_path(&path, Color::new(0., 0., 0., 0.28));
        ui.fill_rect(r.feather(-0.018), (**icon, r.feather(-0.018), ScaleType::Fit, Color::new(1., 1., 1., 0.9)));
    });
}

struct GeneralList {
    icon_lang: SafeTexture,

    lang_btn: ChooseButton,

    #[cfg(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos")))]
    fullscreen_btn: DRectButton,

    cache_btn: DRectButton,
    offline_btn: DRectButton,
    server_status_btn: DRectButton,
    mp_btn: DRectButton,
    mp_addr_btn: DRectButton,
    #[cfg(not(target_env = "ohos"))]
    lowq_btn: DRectButton,
    prefer_reduced_motion_btn: DRectButton,
    insecure_btn: DRectButton,
    enable_anys_btn: DRectButton,
    anys_gateway_btn: DRectButton,

    cache_size: Option<u64>,
    cache_task: Option<Task<Result<u64>>>,
}

impl GeneralList {
    pub fn new(icon_lang: SafeTexture) -> Self {
        let mut this = Self {
            icon_lang,

            lang_btn: ChooseButton::new()
                .with_options(LANG_NAMES.iter().map(|s| s.to_string()).collect())
                .with_selected(
                    get_data()
                        .language
                        .as_ref()
                        .and_then(|it| it.parse::<LanguageIdentifier>().ok())
                        .and_then(|ident| LANG_IDENTS.iter().position(|it| *it == ident))
                        .unwrap_or_default(),
                ),

            #[cfg(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos")))]
            fullscreen_btn: DRectButton::new(),

            cache_btn: DRectButton::new(),
            offline_btn: DRectButton::new(),
            server_status_btn: DRectButton::new(),
            mp_btn: DRectButton::new(),
            mp_addr_btn: DRectButton::new(),
            #[cfg(not(target_env = "ohos"))]
            lowq_btn: DRectButton::new(),
            prefer_reduced_motion_btn: DRectButton::new(),
            insecure_btn: DRectButton::new(),
            enable_anys_btn: DRectButton::new(),
            anys_gateway_btn: DRectButton::new(),
            cache_size: None,
            cache_task: None,
        };
        let _ = this.update_cache_size();
        this
    }

    pub fn top_touch(&mut self, touch: &Touch, t: f32) -> bool {
        if self.lang_btn.top_touch(touch, t) {
            return true;
        }
        false
    }

    fn dir_size(path: impl Into<PathBuf>) -> io::Result<u64> {
        fn inner(mut dir: fs::ReadDir) -> io::Result<u64> {
            dir.try_fold(0, |acc, file| {
                let file = file?;
                let size = match file.metadata()? {
                    data if data.is_dir() => inner(fs::read_dir(file.path())?)?,
                    data => data.len(),
                };
                Ok(acc + size)
            })
        }

        inner(fs::read_dir(path.into())?)
    }

    fn update_cache_size(&mut self) -> Result<()> {
        self.cache_size = None;

        let cache_dir = dir::cache()?;
        self.cache_task = Some(Task::new(async { Ok(Self::dir_size(cache_dir)?) }));
        Ok(())
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;
        if self.lang_btn.touch(touch, t) {
            return Ok(Some(false));
        }

        #[cfg(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos")))]
        if self.fullscreen_btn.touch(touch, t) {
            config.fullscreen_mode ^= true;

            macroquad::window::set_fullscreen(config.fullscreen_mode);

            return Ok(Some(true));
        }

        if self.cache_btn.touch(touch, t) {
            fs::remove_dir_all(dir::cache()?)?;
            self.update_cache_size()?;
            show_message(tl!("item-cache-cleared")).ok();
            return Ok(Some(false));
        }
        if self.offline_btn.touch(touch, t) {
            config.offline_mode ^= true;
            return Ok(Some(true));
        }
        if self.server_status_btn.touch(touch, t) {
            let _ = open_url(STATUS_PAGE);
            return Ok(Some(true));
        }
        if self.mp_btn.touch(touch, t) {
            config.mp_enabled ^= true;
            return Ok(Some(true));
        }
        if self.mp_addr_btn.touch(touch, t) {
            request_input("mp_addr", InputBox::new().default_text(&config.mp_address));
            return Ok(Some(true));
        }
        #[cfg(not(target_env = "ohos"))]
        if self.lowq_btn.touch(touch, t) {
            config.sample_count = if config.sample_count == 1 { 2 } else { 1 };
            return Ok(Some(true));
        }
        if self.prefer_reduced_motion_btn.touch(touch, t) {
            data.prefer_reduced_motion ^= true;
            PREFER_REDUCED_MOTION.store(data.prefer_reduced_motion, Ordering::Relaxed);
            return Ok(Some(true));
        }
        if self.insecure_btn.touch(touch, t) {
            data.accept_invalid_cert ^= true;
            return Ok(Some(true));
        }
        if self.enable_anys_btn.touch(touch, t) {
            data.enable_anys ^= true;
            return Ok(Some(true));
        }
        if self.anys_gateway_btn.touch(touch, t) {
            request_input("anys_gateway", InputBox::new().default_text(&data.anys_gateway));
            return Ok(Some(true));
        }
        Ok(None)
    }

    pub fn update(&mut self, t: f32) -> Result<bool> {
        self.lang_btn.update(t);
        let data = get_data_mut();
        if self.lang_btn.changed() {
            data.language = Some(LANG_IDENTS[self.lang_btn.selected()].to_string());
            sync_data();
            return Ok(true);
        }
        if let Some((id, text)) = take_input() {
            if id == "mp_addr" {
                if let Err(err) = text.to_socket_addrs() {
                    show_error(anyhow::Error::new(err).context(tl!("item-mp-addr-invalid")));
                    return Ok(false);
                } else {
                    data.config.mp_address = text;
                    return Ok(true);
                }
            } else if id == "anys_gateway" {
                if let Err(err) = Url::parse(&text) {
                    show_error(anyhow::Error::new(err).context(tl!("item-anys-gateway-invalid")));
                    return Ok(false);
                } else {
                    data.anys_gateway = text.trim_end_matches('/').to_string();
                    return Ok(true);
                }
            } else {
                return_input(id, text);
            }
        }
        if let Some(task) = &mut self.cache_task {
            if let Some(size) = task.take() {
                self.cache_size = size.ok();
                self.cache_task = None;
            }
        }
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        let mut h = 0.;
        macro_rules! item {
            ($($b:tt)*) => {{
                $($b)*
                ui.dy(ITEM_HEIGHT);
                h += ITEM_HEIGHT;
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;
        item! {
            let rt = render_title(ui, tl!("item-lang"), None);
            let w = 0.06;
            let r = Rect::new(rt + 0.01, (ITEM_HEIGHT - w) / 2., w, w);
            ui.fill_rect(r, (*self.icon_lang, r));
            self.lang_btn.render(ui, rr, t);
        }

        #[cfg(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos")))]
        item! {
            render_title(ui, tl!("item-fullscreen"), None);
            render_switch(ui, rr, t, &mut self.fullscreen_btn, config.fullscreen_mode);
        }

        item! {
            render_title(ui, tl!("item-offline"), Some(tl!("item-offline-sub")));
            render_switch(ui, rr, t, &mut self.offline_btn, config.offline_mode);
        }
        item! {
            render_title(ui, tl!("item-server-status"), Some(tl!("item-server-status-sub")));
            self.server_status_btn.render_text(ui, rr, t, tl!("check-status"), 0.5, true);
        }
        item! {
            render_title(ui, tl!("item-mp"), Some(tl!("item-mp-sub")));
            render_switch(ui, rr, t, &mut self.mp_btn, config.mp_enabled);
        }
        item! {
            render_title(ui, tl!("item-mp-addr"), Some(tl!("item-mp-addr-sub")));
            self.mp_addr_btn.render_text(ui, rr, t, &config.mp_address, 0.4, false);
        }
        item! {
            render_title(ui, tl!("item-prefer-reduced-motion"), Some(tl!("item-prefer-reduced-motion-sub")));
            render_switch(ui, rr, t, &mut self.prefer_reduced_motion_btn, data.prefer_reduced_motion);
        }
        #[cfg(not(target_env = "ohos"))]
        item! {
            render_title(ui, tl!("item-lowq"), Some(tl!("item-lowq-sub")));
            render_switch(ui, rr, t, &mut self.lowq_btn, config.sample_count == 1);
        }
        item! {
            let cache_size = if let Some(size) = self.cache_size {
                Cow::Owned(tl!("item-cache-size", "size" => ByteSize(size).to_string()))
            } else {
                tl!("item-cache-size-loading")
            };
            render_title(ui, tl!("item-clear-cache"), Some(cache_size));
            self.cache_btn.render_text(ui, rr, t, tl!("item-clear-cache-btn"), 0.5, true);
        }
        ui.dy(0.04);
        h += 0.04;
        item! {
            render_title(ui, tl!("item-insecure"), Some(tl!("item-insecure-sub")));
            render_switch(ui, rr, t, &mut self.insecure_btn, data.accept_invalid_cert);
        }
        item! {
            render_title(ui, tl!("item-enable-anys"), Some(tl!("item-enable-anys-sub")));
            render_switch(ui, rr, t, &mut self.enable_anys_btn, data.enable_anys);
        }
        item! {
            render_title(ui, tl!("item-anys-gateway"), Some(tl!("item-anys-gateway-sub")));
            self.anys_gateway_btn.render_text(ui, rr, t, &data.anys_gateway, 0.4, false);
        }
        self.lang_btn.render_top(ui, t, 1.);
        (w, h)
    }
}

struct AudioList {
    adjust_btn: DRectButton,
    music_slider: Slider,
    sfx_slider: Slider,
    bgm_slider: Slider,
    cali_btn: DRectButton,
    #[cfg(not(target_os = "android"))]
    preferred_sample_rate_btn: DRectButton,
    #[cfg(target_env = "ohos")]
    audio_buffer_size_btn: DRectButton,
    cali_task: LocalTask<Result<OffsetPage>>,
    next_page: Option<NextPage>,
}

impl AudioList {
    pub fn new() -> Self {
        Self {
            adjust_btn: DRectButton::new(),
            music_slider: Slider::new(0.0..2.0, 0.05),
            sfx_slider: Slider::new(0.0..2.0, 0.05),
            bgm_slider: Slider::new(0.0..2.0, 0.05),
            cali_btn: DRectButton::new(),
            #[cfg(not(target_os = "android"))]
            preferred_sample_rate_btn: DRectButton::new(),
            #[cfg(target_env = "ohos")]
            audio_buffer_size_btn: DRectButton::new(),

            cali_task: None,
            next_page: None,
        }
    }

    pub fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;
        if self.adjust_btn.touch(touch, t) {
            config.adjust_time ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.music_slider.touch(touch, t, &mut config.volume_music) {
            return Ok(wt);
        }
        if let wt @ Some(_) = self.sfx_slider.touch(touch, t, &mut config.volume_sfx) {
            return Ok(wt);
        }
        let old = config.volume_bgm;
        if let wt @ Some(_) = self.bgm_slider.touch(touch, t, &mut config.volume_bgm) {
            if (config.volume_bgm - old).abs() > 0.001 {
                BGM_VOLUME_UPDATED.store(true, Ordering::Relaxed);
            }
            return Ok(wt);
        }
        if self.cali_btn.touch(touch, t) {
            self.cali_task = Some(Box::pin(OffsetPage::new()));
            return Ok(Some(false));
        }
        #[cfg(not(target_os = "android"))]
        if self.preferred_sample_rate_btn.touch(touch, t) {
            let options = [None, Some(44100), Some(48000), Some(88200), Some(96000), Some(192000)];
            let current = config.preferred_sample_rate;
            let selected = options.iter().position(|&r| r == current).unwrap_or(0);
            config.preferred_sample_rate = options[(selected + 1) % options.len()];
            return Ok(Some(true));
        }
        #[cfg(target_env = "ohos")]
        if self.audio_buffer_size_btn.touch(touch, t) {
            let options = [128u32, 256u32, 512u32];
            let current = config.audio_buffer_size.unwrap_or(256);
            let selected = options.iter().position(|&r| r == current).unwrap_or(1);
            config.audio_buffer_size = Some(options[(selected + 1) % options.len()]);
            return Ok(Some(true));
        }
        Ok(None)
    }

    pub fn update(&mut self, _t: f32) -> Result<bool> {
        if let Some(task) = &mut self.cali_task {
            if let Some(res) = poll_future(task.as_mut()) {
                match res {
                    Err(err) => show_error(err.context(tl!("load-cali-failed"))),
                    Ok(page) => {
                        self.next_page = Some(NextPage::Overlay(Box::new(page)));
                    }
                }
                self.cali_task = None;
            }
        }
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        let mut h = 0.;
        macro_rules! item {
            ($($b:tt)*) => {{
                $($b)*
                ui.dy(ITEM_HEIGHT);
                h += ITEM_HEIGHT;
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;
        item! {
            render_title(ui, tl!("item-adjust"), Some(tl!("item-adjust-sub")));
            render_switch(ui, rr, t, &mut self.adjust_btn, config.adjust_time);
        }
        item! {
            render_title(ui, tl!("item-music"), None);
            self.music_slider.render(ui, rr, t, config.volume_music, format!("{:.2}", config.volume_music));
        }
        item! {
            render_title(ui, tl!("item-sfx"), None);
            self.sfx_slider.render(ui, rr, t, config.volume_sfx, format!("{:.2}", config.volume_sfx));
        }
        item! {
            render_title(ui, tl!("item-bgm"), None);
            self.bgm_slider.render(ui, rr, t, config.volume_bgm, format!("{:.2}", config.volume_bgm));
        }
        item! {
            render_title(ui, tl!("item-cali"), None);
            self.cali_btn.render_text(ui, rr, t, format!("{:.0}ms", config.offset * 1000.), 0.5, true);
        }
        #[cfg(not(target_os = "android"))]
        item! {
            render_title(ui, tl!("item-preferred-sample-rate"), None);
            let text = if let Some(rate) = config.preferred_sample_rate {
                format!("{} Hz", rate)
            } else {
                tl!("preferred-sample-rate-default").to_string()
            };
            self.preferred_sample_rate_btn.render_text(ui, rr, t, text, 0.5, false);
        }
        #[cfg(target_env = "ohos")]
        item! {
            render_title(ui, tl!("item-audio-buffer-size"), None);
            let buf_size = config.audio_buffer_size.unwrap_or(256);
            self.audio_buffer_size_btn.render_text(ui, rr, t, format!("{}", buf_size), 0.5, false);
        }
        (w, h)
    }

    pub fn next_page(&mut self) -> Option<NextPage> {
        self.next_page.take()
    }
}

struct ChartList {
    show_acc_btn: DRectButton,
    ap_fc_indicator_btn: DRectButton,
    show_avg_fps_btn: DRectButton,
    dc_pause_btn: DRectButton,
    dhint_btn: DRectButton,
    opt_btn: DRectButton,
    use_keyboard_btn: DRectButton,
    speed_slider: Slider,
    size_slider: Slider,
}

impl ChartList {
    pub fn new() -> Self {
        Self {
            show_acc_btn: DRectButton::new(),
            ap_fc_indicator_btn: DRectButton::new(),
            show_avg_fps_btn: DRectButton::new(),
            dc_pause_btn: DRectButton::new(),
            dhint_btn: DRectButton::new(),
            opt_btn: DRectButton::new(),
            use_keyboard_btn: DRectButton::new(),
            speed_slider: Slider::new(0.5..2., 0.05),
            size_slider: Slider::new(0.8..1.2, 0.005),
        }
    }

    pub fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;
        if self.show_acc_btn.touch(touch, t) {
            config.show_acc ^= true;
            return Ok(Some(true));
        }
        if self.ap_fc_indicator_btn.touch(touch, t) {
            config.ap_fc_indicator ^= true;
            return Ok(Some(true));
        }
        if self.show_avg_fps_btn.touch(touch, t) {
            config.show_avg_fps ^= true;
            return Ok(Some(true));
        }
        if self.dc_pause_btn.touch(touch, t) {
            config.double_click_to_pause ^= true;
            return Ok(Some(true));
        }
        if self.dhint_btn.touch(touch, t) {
            config.double_hint ^= true;
            return Ok(Some(true));
        }
        if self.opt_btn.touch(touch, t) {
            config.aggressive ^= true;
            return Ok(Some(true));
        }
        if self.use_keyboard_btn.touch(touch, t) {
            config.use_keyboard ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.speed_slider.touch(touch, t, &mut config.speed) {
            return Ok(wt);
        }
        if let wt @ Some(_) = self.size_slider.touch(touch, t, &mut config.note_scale) {
            return Ok(wt);
        }
        Ok(None)
    }

    pub fn update(&mut self, _t: f32) -> Result<bool> {
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        let mut h = 0.;
        macro_rules! item {
            ($($b:tt)*) => {{
                $($b)*
                ui.dy(ITEM_HEIGHT);
                h += ITEM_HEIGHT;
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;
        item! {
            render_title(ui, tl!("item-show-acc"), None);
            render_switch(ui, rr, t, &mut self.show_acc_btn, config.show_acc);
        }
        item! {
            render_title(ui, tl!("item-ap-fc-indicator"), Some(tl!("item-ap-fc-indicator-sub")));
            render_switch(ui, rr, t, &mut self.ap_fc_indicator_btn, config.ap_fc_indicator);
        }
        item! {
            render_title(ui, tl!("item-show-avg-fps"), Some(tl!("item-show-avg-fps-sub")));
            render_switch(ui, rr, t, &mut self.show_avg_fps_btn, config.show_avg_fps);
        }
        item! {
            render_title(ui, tl!("item-dc-pause"), None);
            render_switch(ui, rr, t, &mut self.dc_pause_btn, config.double_click_to_pause);
        }
        item! {
            render_title(ui, tl!("item-dhint"), Some(tl!("item-dhint-sub")));
            render_switch(ui, rr, t, &mut self.dhint_btn, config.double_hint);
        }
        item! {
            render_title(ui, tl!("item-opt"), Some(tl!("item-opt-sub")));
            render_switch(ui, rr, t, &mut self.opt_btn, config.aggressive);
        }
        item! {
            render_title(ui, tl!("item-use-keyboard"), Some(tl!("item-use-keyboard-sub")));
            render_switch(ui, rr, t, &mut self.use_keyboard_btn, config.use_keyboard);
        }
        item! {
            render_title(ui, tl!("item-speed"), None);
            self.speed_slider.render(ui, rr, t, config.speed, format!("{:.2}", config.speed));
        }
        item! {
            render_title(ui, tl!("item-note-size"), None);
            self.size_slider.render(ui, rr, t, config.note_scale, format!("{:.3}", config.note_scale));
        }
        (w, h)
    }
}

struct DebugList {
    chart_debug_btn: DRectButton,
    touch_debug_btn: DRectButton,
}

impl DebugList {
    pub fn new() -> Self {
        Self {
            chart_debug_btn: DRectButton::new(),
            touch_debug_btn: DRectButton::new(),
        }
    }

    pub fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;
        if self.chart_debug_btn.touch(touch, t) {
            config.chart_debug ^= true;
            return Ok(Some(true));
        }
        if self.touch_debug_btn.touch(touch, t) {
            config.touch_debug ^= true;
            return Ok(Some(true));
        }
        Ok(None)
    }

    pub fn update(&mut self, _t: f32) -> Result<bool> {
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        let mut h = 0.;
        macro_rules! item {
            ($($b:tt)*) => {{
                $($b)*
                ui.dy(ITEM_HEIGHT);
                h += ITEM_HEIGHT;
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;
        item! {
            render_title(ui, tl!("item-chart-debug"), Some(tl!("item-chart-debug-sub")));
            render_switch(ui, rr, t, &mut self.chart_debug_btn, config.chart_debug);
        }
        item! {
            render_title(ui, tl!("item-touch-debug"), Some(tl!("item-touch-debug-sub")));
            render_switch(ui, rr, t, &mut self.touch_debug_btn, config.touch_debug);
        }
        (w, h)
    }
}

// ===========================================================================
// DGHub 联动设置 (独立 Tab)
// ===========================================================================

struct DghubList {
    github_icon: SafeTexture,
    browser_icon: SafeTexture,
    qq_icon: SafeTexture,
    author_btn: DRectButton,
    repo_btn: DRectButton,
    site_btn: DRectButton,
    qq_btn: DRectButton,
    enable_btn: DRectButton,
    host_btn: DRectButton,
    port_btn: DRectButton,
    token_btn: DRectButton,
    reconnect_btn: DRectButton,
    use_phira_btn: DRectButton,
    indicator_style_btn: DRectButton,
    clear_strength_on_pause_btn: DRectButton,
    miss_enable_btn: DRectButton,
    miss_strength_slider: Slider,
    miss_duration_slider: Slider,
    miss_preset_btn: ChooseButton,
    miss_channel_btn: DRectButton,
    bad_enable_btn: DRectButton,
    bad_strength_slider: Slider,
    bad_duration_slider: Slider,
    bad_preset_btn: ChooseButton,
    bad_channel_btn: DRectButton,
    good_enable_btn: DRectButton,
    good_strength_slider: Slider,
    good_duration_slider: Slider,
    good_preset_btn: ChooseButton,
    good_channel_btn: DRectButton,
    perf_enable_btn: DRectButton,
    perf_strength_slider: Slider,
    perf_duration_slider: Slider,
    perf_preset_btn: ChooseButton,
    perf_channel_btn: DRectButton,
    throttle_btn: DRectButton,
}

impl DghubList {
    pub fn new(github_icon: SafeTexture, browser_icon: SafeTexture, qq_icon: SafeTexture) -> Self {
        let b = || DRectButton::new();
        let cfg = &get_data().config;
        Self {
            github_icon,
            browser_icon,
            qq_icon,
            author_btn: b(),
            repo_btn: b(),
            site_btn: b(),
            qq_btn: b(),
            enable_btn: b(),
            host_btn: b(),
            port_btn: b(),
            token_btn: b(),
            reconnect_btn: b(),
            use_phira_btn: b(),
            indicator_style_btn: b(),
            clear_strength_on_pause_btn: b(),
            miss_enable_btn: b(),
            miss_strength_slider: Slider::new(0.0..100.0, 1.0),
            miss_duration_slider: Slider::new(0.0..3.0, 0.1),
            miss_preset_btn: dghub_preset_button(&cfg.dghub_miss_preset),
            miss_channel_btn: b(),
            bad_enable_btn: b(),
            bad_strength_slider: Slider::new(0.0..100.0, 1.0),
            bad_duration_slider: Slider::new(0.0..3.0, 0.1),
            bad_preset_btn: dghub_preset_button(&cfg.dghub_bad_preset),
            bad_channel_btn: b(),
            good_enable_btn: b(),
            good_strength_slider: Slider::new(0.0..100.0, 1.0),
            good_duration_slider: Slider::new(0.0..3.0, 0.1),
            good_preset_btn: dghub_preset_button(&cfg.dghub_good_preset),
            good_channel_btn: b(),
            perf_enable_btn: b(),
            perf_strength_slider: Slider::new(0.0..100.0, 1.0),
            perf_duration_slider: Slider::new(0.0..3.0, 0.1),
            perf_preset_btn: dghub_preset_button(&cfg.dghub_perfect_preset),
            perf_channel_btn: b(),
            throttle_btn: b(),
        }
    }

    pub fn top_touch(&mut self, touch: &Touch, t: f32) -> bool {
        if self.miss_preset_btn.top_touch(touch, t) {
            return true;
        }
        if self.bad_preset_btn.top_touch(touch, t) {
            return true;
        }
        if self.good_preset_btn.top_touch(touch, t) {
            return true;
        }
        if self.perf_preset_btn.top_touch(touch, t) {
            return true;
        }
        false
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let cfg = &mut data.config;
        if self.author_btn.touch(touch, t) {
            let _ = open_url(DGHUB_AUTHOR_URL);
            return Ok(Some(false));
        }
        if self.repo_btn.touch(touch, t) {
            let _ = open_url(DGHUB_REPO_URL);
            return Ok(Some(false));
        }
        if self.site_btn.touch(touch, t) {
            let _ = open_url(DGHUB_SITE_URL);
            return Ok(Some(false));
        }
        if self.qq_btn.touch(touch, t) {
            let _ = open_url(DGHUB_QQ_URL);
            return Ok(Some(false));
        }
        macro_rules! sw {
            ($b:ident, $f:ident) => {
                if self.$b.touch(touch, t) {
                    cfg.$f ^= true;
                    return Ok(Some(true));
                }
            };
        }
        macro_rules! ti {
            ($b:ident, $id:literal, $d:expr) => {
                if self.$b.touch(touch, t) {
                    request_input($id, InputBox::new().default_text($d));
                    return Ok(Some(true));
                }
            };
        }
        macro_rules! ch {
            ($b:ident, $f:ident) => {
                if self.$b.touch(touch, t) {
                    cfg.$f = next_dghub_channel(&cfg.$f);
                    return Ok(Some(true));
                }
            };
        }
        sw!(enable_btn, dghub_enable);
        ti!(host_btn, "dghub_host", &cfg.dghub_host);
        ti!(port_btn, "dghub_port", &cfg.dghub_port.to_string());
        ti!(token_btn, "dghub_token", &cfg.dghub_token);
        if self.reconnect_btn.touch(touch, t) {
            crate::dghub::request_reconnect();
            return Ok(Some(false));
        }
        sw!(use_phira_btn, dghub_use_phira_config);
        if self.indicator_style_btn.touch(touch, t) {
            cfg.dghub_indicator_style = next_dghub_indicator_style(&cfg.dghub_indicator_style);
            return Ok(Some(true));
        }
        sw!(clear_strength_on_pause_btn, dghub_clear_strength_on_pause);
        if cfg.dghub_use_phira_config {
            macro_rules! sl {
                ($slider:ident, $field:ident, u32) => {
                    if let wt @ Some(_) = touch_u32_slider(&mut self.$slider, touch, t, &mut cfg.$field) {
                        return Ok(wt);
                    }
                };
                ($slider:ident, $field:ident, f64) => {
                    if let wt @ Some(_) = touch_f64_slider(&mut self.$slider, touch, t, &mut cfg.$field) {
                        return Ok(wt);
                    }
                };
            }
            sw!(miss_enable_btn, dghub_miss_enable);
            sl!(miss_strength_slider, dghub_miss_strength, u32);
            sl!(miss_duration_slider, dghub_miss_duration, f64);
            if self.miss_preset_btn.touch(touch, t) {
                return Ok(Some(false));
            }
            ch!(miss_channel_btn, dghub_miss_channel);
            sw!(bad_enable_btn, dghub_bad_enable);
            sl!(bad_strength_slider, dghub_bad_strength, u32);
            sl!(bad_duration_slider, dghub_bad_duration, f64);
            if self.bad_preset_btn.touch(touch, t) {
                return Ok(Some(false));
            }
            ch!(bad_channel_btn, dghub_bad_channel);
            sw!(good_enable_btn, dghub_good_enable);
            sl!(good_strength_slider, dghub_good_strength, u32);
            sl!(good_duration_slider, dghub_good_duration, f64);
            if self.good_preset_btn.touch(touch, t) {
                return Ok(Some(false));
            }
            ch!(good_channel_btn, dghub_good_channel);
            sw!(perf_enable_btn, dghub_perfect_enable);
            sl!(perf_strength_slider, dghub_perfect_strength, u32);
            sl!(perf_duration_slider, dghub_perfect_duration, f64);
            if self.perf_preset_btn.touch(touch, t) {
                return Ok(Some(false));
            }
            ch!(perf_channel_btn, dghub_perfect_channel);
            ti!(throttle_btn, "dth", &cfg.dghub_throttle_ms.to_string());
        }
        Ok(None)
    }

    pub fn update(&mut self, _t: f32) -> Result<bool> {
        self.miss_preset_btn.update(_t);
        self.bad_preset_btn.update(_t);
        self.good_preset_btn.update(_t);
        self.perf_preset_btn.update(_t);
        if get_data().config.dghub_use_phira_config {
            macro_rules! preset_changed {
                ($btn:ident, $field:ident) => {
                    if self.$btn.changed() {
                        get_data_mut().config.$field = dghub_preset_value(self.$btn.selected());
                        return Ok(true);
                    }
                };
            }
            preset_changed!(miss_preset_btn, dghub_miss_preset);
            preset_changed!(bad_preset_btn, dghub_bad_preset);
            preset_changed!(good_preset_btn, dghub_good_preset);
            preset_changed!(perf_preset_btn, dghub_perfect_preset);
        }
        if let Some((id, text)) = take_input() {
            let cfg = &mut get_data_mut().config;
            match id.as_str() {
                "dghub_host" => {
                    cfg.dghub_host = text.trim().to_string();
                    return Ok(true);
                }
                "dghub_port" => {
                    if let Ok(p) = text.trim().parse() {
                        cfg.dghub_port = p;
                        return Ok(true);
                    }
                }
                "dghub_token" => {
                    cfg.dghub_token = text.trim().to_string();
                    return Ok(true);
                }
                _ => {}
            }
            if cfg.dghub_use_phira_config {
                let t = text.trim();
                macro_rules! p {
                    ($id:literal, $field:ident, u32) => {
                        if id == $id {
                            if let Ok(n) = t.parse() {
                                cfg.$field = n;
                                return Ok(true);
                            }
                        }
                    };
                    ($id:literal, $field:ident, f64) => {
                        if id == $id {
                            if let Ok(n) = t.parse() {
                                cfg.$field = n;
                                return Ok(true);
                            }
                        }
                    };
                    ($id:literal, $field:ident, str) => {
                        if id == $id {
                            cfg.$field = t.to_string();
                            return Ok(true);
                        }
                    };
                }
                p!("dms", dghub_miss_strength, u32);
                p!("dmd", dghub_miss_duration, f64);
                p!("dbs", dghub_bad_strength, u32);
                p!("dbd", dghub_bad_duration, f64);
                p!("dgs", dghub_good_strength, u32);
                p!("dgd", dghub_good_duration, f64);
                p!("dps", dghub_perfect_strength, u32);
                p!("dpd", dghub_perfect_duration, f64);
                p!("dth", dghub_throttle_ms, u32);
            }
        }
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        let mut h = 0.;
        macro_rules! item { ($($b:tt)*) => {{ $($b)* ui.dy(ITEM_HEIGHT); h += ITEM_HEIGHT; }} }
        let rr = right_rect(w);
        let cfg = &get_data().config;

        item! { render_title(ui, tl!("item-dghub-author"), None);
        render_link_button(ui, icon_rect(rr), t, &mut self.author_btn, &self.github_icon); }
        item! { render_title(ui, tl!("item-dghub-project"), None);
        render_link_button(ui, icon_rect(rr), t, &mut self.repo_btn, &self.github_icon); }
        item! { render_title(ui, tl!("item-dghub-link"), None);
        render_link_button(ui, icon_rect(rr), t, &mut self.site_btn, &self.browser_icon); }
        item! { render_title(ui, tl!("item-dghub-qq"), None);
        render_link_button(ui, icon_rect(rr), t, &mut self.qq_btn, &self.qq_icon); }
        item! { render_title(ui, tl!("item-dghub-note"), Some(tl!("item-dghub-note-sub"))); }

        // 外部连接
        item! { render_title(ui, tl!("item-dghub"), Some(tl!("item-dghub-sub")));
        render_switch(ui, rr, t, &mut self.enable_btn, cfg.dghub_enable); }
        item! { render_title(ui, tl!("item-dghub-host"), Some(tl!("item-dghub-host-sub")));
        self.host_btn.render_text(ui, rr, t, &cfg.dghub_host, 0.4, false); }
        item! { render_title(ui, tl!("item-dghub-port"), Some(tl!("item-dghub-port-sub")));
        self.port_btn.render_text(ui, rr, t, cfg.dghub_port.to_string(), 0.4, false); }
        item! { render_title(ui, tl!("item-dghub-token"), Some(tl!("item-dghub-token-sub")));
        self.token_btn.render_text(ui, rr, t, if cfg.dghub_token.is_empty(){"—"}else{&cfg.dghub_token}, 0.4, false); }
        item! {
            let st = crate::dghub::connection_status();
            let txt = match st { 1=>tl!("item-dghub-status-connecting"), 2=>tl!("item-dghub-status-connected"), _=>tl!("item-dghub-status-off") };
            render_title(ui, tl!("item-dghub-status"), Some(txt));
            self.reconnect_btn.render_text(ui, rr, t, tl!("item-dghub-reconnect"), 0.4, true);
        }
        item! { render_title(ui, tl!("item-dghub-source"), Some(tl!("item-dghub-source-sub")));
        self.use_phira_btn.render_text(ui, rr, t, if cfg.dghub_use_phira_config { tl!("item-dghub-source-phira") } else { tl!("item-dghub-source-dghub") }, 0.35, cfg.dghub_use_phira_config); }
        item! { render_title(ui, tl!("item-dghub-indicator-style"), None);
        self.indicator_style_btn.render_text(ui, rr, t, dghub_indicator_style_label(&cfg.dghub_indicator_style), 0.4, false); }
        item! { render_title(ui, tl!("item-dghub-clear-strength-on-pause"), Some(tl!("item-dghub-clear-strength-on-pause-sub")));
        render_switch(ui, rr, t, &mut self.clear_strength_on_pause_btn, cfg.dghub_clear_strength_on_pause); }
        if cfg.dghub_use_phira_config {
            macro_rules! grade {
                ($name:literal, $enable_btn:ident, $enable:ident, $strength_slider:ident, $strength:ident, $duration_slider:ident, $duration:ident, $preset_btn:ident, $preset:ident) => {{
                    item! { render_title(ui, format!("{} {}", $name, tl!("item-dghub")), None);
                    render_switch(ui, rr, t, &mut self.$enable_btn, cfg.$enable); }
                    item! { render_title(ui, format!("{} {}", $name, tl!("item-dghub-strength")), None);
                    self.$strength_slider.render(ui, rr, t, cfg.$strength as f32, cfg.$strength.to_string()); }
                    item! { render_title(ui, format!("{} {}", $name, tl!("item-dghub-duration")), None);
                    self.$duration_slider.render(ui, rr, t, cfg.$duration as f32, format!("{:.1}", cfg.$duration)); }
                    item! { render_title(ui, format!("{} {}", $name, tl!("item-dghub-preset")), None);
                    self.$preset_btn.render(ui, rr, t); }
                }};
            }
            grade!(
                "Miss",
                miss_enable_btn,
                dghub_miss_enable,
                miss_strength_slider,
                dghub_miss_strength,
                miss_duration_slider,
                dghub_miss_duration,
                miss_preset_btn,
                dghub_miss_preset
            );
            grade!(
                "Bad",
                bad_enable_btn,
                dghub_bad_enable,
                bad_strength_slider,
                dghub_bad_strength,
                bad_duration_slider,
                dghub_bad_duration,
                bad_preset_btn,
                dghub_bad_preset
            );
            grade!(
                "Good",
                good_enable_btn,
                dghub_good_enable,
                good_strength_slider,
                dghub_good_strength,
                good_duration_slider,
                dghub_good_duration,
                good_preset_btn,
                dghub_good_preset
            );
            grade!(
                "Perfect",
                perf_enable_btn,
                dghub_perfect_enable,
                perf_strength_slider,
                dghub_perfect_strength,
                perf_duration_slider,
                dghub_perfect_duration,
                perf_preset_btn,
                dghub_perfect_preset
            );
            item! { render_title(ui, tl!("item-dghub-throttle"), None);
            self.throttle_btn.render_text(ui, rr, t, cfg.dghub_throttle_ms.to_string(), 0.4, false); }
            item! { render_title(ui, format!("Miss {}", tl!("item-dghub-channel")), None);
            self.miss_channel_btn.render_text(ui, rr, t, dghub_channel_label(&cfg.dghub_miss_channel), 0.4, false); }
            item! { render_title(ui, format!("Bad {}", tl!("item-dghub-channel")), None);
            self.bad_channel_btn.render_text(ui, rr, t, dghub_channel_label(&cfg.dghub_bad_channel), 0.4, false); }
            item! { render_title(ui, format!("Good {}", tl!("item-dghub-channel")), None);
            self.good_channel_btn.render_text(ui, rr, t, dghub_channel_label(&cfg.dghub_good_channel), 0.4, false); }
            item! { render_title(ui, format!("Perfect {}", tl!("item-dghub-channel")), None);
            self.perf_channel_btn.render_text(ui, rr, t, dghub_channel_label(&cfg.dghub_perfect_channel), 0.4, false); }
            self.miss_preset_btn.render_top(ui, t, 1.);
            self.bad_preset_btn.render_top(ui, t, 1.);
            self.good_preset_btn.render_top(ui, t, 1.);
            self.perf_preset_btn.render_top(ui, t, 1.);
        }
        (w, h)
    }
}
