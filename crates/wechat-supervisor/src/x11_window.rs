use anyhow::Result;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _, EventMask,
    Window,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::health::SharedState;

const MIN_MAIN_WIDTH: u16 = 520;
const MIN_MAIN_HEIGHT: u16 = 420;
const MIN_LOGIN_WIDTH: u32 = 200;
const MIN_LOGIN_HEIGHT: u32 = 200;
const GEOMETRY_TOLERANCE_PX: i32 = 2;

// EWMH _NET_MOVERESIZE_WINDOW flags: NorthWest gravity (1) + X|Y|W|H (bits 8..11) + pager source (2 << 12).
const MOVERESIZE_FLAGS: u32 = 1 | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11) | (2 << 12);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SizeHints {
    pub min_width: Option<u32>,
    pub min_height: Option<u32>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
}

impl SizeHints {
    pub fn is_fixed_size(&self) -> bool {
        let fixed_w =
            matches!((self.min_width, self.max_width), (Some(a), Some(b)) if a > 0 && a == b);
        let fixed_h =
            matches!((self.min_height, self.max_height), (Some(a), Some(b)) if a > 0 && a == b);
        fixed_w && fixed_h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryWindowType {
    Normal,
    Dialog,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCandidate {
    pub window: Window,
    pub wm_class: String,
    pub primary_type: PrimaryWindowType,
    pub is_transient: bool,
    pub has_auxiliary_state: bool,
    pub width: u16,
    pub height: u16,
    pub hints: SizeHints,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitTargetKind {
    Main,
    LoginDialog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FitSelection {
    pub window: Window,
    pub kind: FitTargetKind,
    pub geometry: TargetGeometry,
}

fn is_wechat_class(wm_class: &str) -> bool {
    wm_class.to_ascii_lowercase().contains("wechat")
}

fn is_main_window_size(width: u16, height: u16, hints: SizeHints) -> bool {
    let max_too_small = hints
        .max_width
        .is_some_and(|w| w > 0 && w < MIN_MAIN_WIDTH as u32)
        || hints
            .max_height
            .is_some_and(|h| h > 0 && h < MIN_MAIN_HEIGHT as u32);
    if max_too_small {
        return false;
    }
    let geom_ok = width >= MIN_MAIN_WIDTH && height >= MIN_MAIN_HEIGHT;
    let min_hints_ok = hints.min_width.unwrap_or(0) >= MIN_MAIN_WIDTH as u32
        && hints.min_height.unwrap_or(0) >= MIN_MAIN_HEIGHT as u32;
    geom_ok || min_hints_ok
}

fn is_main_window_candidate(cand: &WindowCandidate) -> bool {
    is_wechat_class(&cand.wm_class)
        && cand.primary_type == PrimaryWindowType::Normal
        && !cand.is_transient
        && !cand.has_auxiliary_state
        && !cand.hints.is_fixed_size()
        && is_main_window_size(cand.width, cand.height, cand.hints)
}

fn is_login_dialog_candidate(cand: &WindowCandidate) -> bool {
    if !is_wechat_class(&cand.wm_class)
        || !matches!(
            cand.primary_type,
            PrimaryWindowType::Normal | PrimaryWindowType::Dialog
        )
        || cand.is_transient
        || cand.has_auxiliary_state
        || !cand.hints.is_fixed_size()
    {
        return false;
    }
    let w = cand.hints.min_width.unwrap_or(cand.width as u32);
    let h = cand.hints.min_height.unwrap_or(cand.height as u32);
    w >= MIN_LOGIN_WIDTH && h >= MIN_LOGIN_HEIGHT
}

pub fn select_window_to_fit(
    candidates: &[WindowCandidate],
    tracked_main: Option<Window>,
    viewport_w: u16,
    viewport_h: u16,
) -> Option<FitSelection> {
    let main_candidates: Vec<&WindowCandidate> = candidates
        .iter()
        .filter(|cand| is_main_window_candidate(cand))
        .collect();

    if !main_candidates.is_empty() {
        let chosen = tracked_main
            .and_then(|id| main_candidates.iter().copied().find(|c| c.window == id))
            .unwrap_or(main_candidates[0]);
        return Some(FitSelection {
            window: chosen.window,
            kind: FitTargetKind::Main,
            geometry: compute_window_geometry(
                viewport_w,
                viewport_h,
                chosen.width,
                chosen.height,
                chosen.hints,
            ),
        });
    }

    let mut best_login: Option<(&WindowCandidate, u64)> = None;
    for cand in candidates.iter().filter(|c| is_login_dialog_candidate(c)) {
        let w = cand.hints.min_width.unwrap_or(cand.width as u32) as u64;
        let h = cand.hints.min_height.unwrap_or(cand.height as u32) as u64;
        let area = w.saturating_mul(h);
        if best_login.is_none_or(|(_, best_area)| area > best_area) {
            best_login = Some((cand, area));
        }
    }

    best_login.map(|(chosen, _)| FitSelection {
        window: chosen.window,
        kind: FitTargetKind::LoginDialog,
        geometry: compute_window_geometry(
            viewport_w,
            viewport_h,
            chosen.width,
            chosen.height,
            chosen.hints,
        ),
    })
}

pub fn compute_window_geometry(
    viewport_w: u16,
    viewport_h: u16,
    current_w: u16,
    current_h: u16,
    hints: SizeHints,
) -> TargetGeometry {
    if hints.is_fixed_size() {
        let w = hints
            .min_width
            .unwrap_or(current_w as u32)
            .min(viewport_w as u32);
        let h = hints
            .min_height
            .unwrap_or(current_h as u32)
            .min(viewport_h as u32);
        let x = ((viewport_w as i32) - (w as i32)).max(0) / 2;
        let y = ((viewport_h as i32) - (h as i32)).max(0) / 2;
        return TargetGeometry {
            x,
            y,
            width: w.max(1),
            height: h.max(1),
        };
    }

    let mut w = viewport_w as u32;
    let mut h = viewport_h as u32;
    if let Some(min_w) = hints.min_width {
        w = w.max(min_w.min(viewport_w as u32));
    }
    if let Some(min_h) = hints.min_height {
        h = h.max(min_h.min(viewport_h as u32));
    }
    if let Some(max_w) = hints.max_width.filter(|&v| v > 0) {
        w = w.min(max_w);
    }
    if let Some(max_h) = hints.max_height.filter(|&v| v > 0) {
        h = h.min(max_h);
    }

    TargetGeometry {
        x: ((viewport_w as i32) - (w as i32)).max(0) / 2,
        y: ((viewport_h as i32) - (h as i32)).max(0) / 2,
        width: w.max(1),
        height: h.max(1),
    }
}

pub fn geometry_matches(
    abs_x: i32,
    abs_y: i32,
    width: u16,
    height: u16,
    target: TargetGeometry,
) -> bool {
    (abs_x - target.x).abs() <= GEOMETRY_TOLERANCE_PX
        && (abs_y - target.y).abs() <= GEOMETRY_TOLERANCE_PX
        && (width as i32 - target.width as i32).abs() <= GEOMETRY_TOLERANCE_PX
        && (height as i32 - target.height as i32).abs() <= GEOMETRY_TOLERANCE_PX
}

pub fn spawn_watcher(state: Arc<SharedState>, _show_tray: bool) -> Result<JoinHandle<()>> {
    let handle = std::thread::Builder::new()
        .name("x11-autofit".to_string())
        .spawn(move || watcher_loop(state))?;
    Ok(handle)
}

pub fn fit_once(_show_tray: bool) -> Result<()> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;
    let atoms = Atoms::intern(&conn)?;
    let mut tracked_main = None;
    let mut centered_dialogs = HashSet::new();
    let _ = fit_wechat_windows(
        &conn,
        root,
        &atoms,
        true,
        true,
        &mut tracked_main,
        &mut centered_dialogs,
    );
    Ok(())
}

fn watcher_loop(state: Arc<SharedState>) {
    while !state.shutdown.load(Ordering::Relaxed) {
        match x11rb::connect(None) {
            Ok((conn, screen_num)) => {
                if let Err(err) = run_connected_loop(&conn, screen_num, &state) {
                    eprintln!("[wechat-x11] Disconnected from X11: {err}");
                }
                state.set_display_state(false, None);
            }
            Err(_) => {
                state.set_display_state(false, None);
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run_connected_loop(conn: &RustConnection, screen_num: usize, state: &SharedState) -> Result<()> {
    let root = conn.setup().roots[screen_num].root;
    let atoms = Atoms::intern(conn)?;

    let _ = conn.randr_select_input(root, NotifyMask::SCREEN_CHANGE);
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new()
            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::SUBSTRUCTURE_NOTIFY),
    )?;
    conn.flush()?;

    let mut last_viewport: Option<(u16, u16)> = None;
    let mut last_fit = Instant::now() - Duration::from_secs(10);
    let mut tracked_main: Option<Window> = None;
    let mut centered_dialogs: HashSet<Window> = HashSet::new();

    while !state.shutdown.load(Ordering::Relaxed) {
        let explicit_fit = state.fit_requested.swap(false, Ordering::SeqCst);
        let mut need_fit = explicit_fit;
        let mut viewport_changed = false;

        while let Some(event) = conn.poll_for_event()? {
            match event {
                Event::RandrScreenChangeNotify(_) => {
                    need_fit = true;
                    viewport_changed = true;
                }
                Event::PropertyNotify(ev) if ev.atom == atoms.net_client_list => {
                    need_fit = true;
                }
                _ => {}
            }
        }

        if let Ok(geom) = conn.get_geometry(root)?.reply() {
            let vp = (geom.width, geom.height);
            state.set_display_state(true, Some(vp));
            if last_viewport != Some(vp) {
                last_viewport = Some(vp);
                need_fit = true;
                viewport_changed = true;
            }
        }

        if viewport_changed {
            centered_dialogs.clear();
        }

        if need_fit && (explicit_fit || last_fit.elapsed() >= Duration::from_millis(350)) {
            let _ = fit_wechat_windows(
                conn,
                root,
                &atoms,
                explicit_fit,
                explicit_fit || viewport_changed,
                &mut tracked_main,
                &mut centered_dialogs,
            );
            last_fit = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

struct Atoms {
    net_client_list: Atom,
    net_wm_window_type: Atom,
    net_wm_window_type_normal: Atom,
    net_wm_window_type_dialog: Atom,
    net_active_window: Atom,
    net_moveresize_window: Atom,
    net_wm_state: Atom,
    net_wm_state_maximized_horz: Atom,
    net_wm_state_maximized_vert: Atom,
    net_wm_state_skip_taskbar: Atom,
    net_wm_state_skip_pager: Atom,
    net_wm_state_modal: Atom,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Self> {
        let intern =
            |name: &[u8]| -> Result<Atom> { Ok(conn.intern_atom(false, name)?.reply()?.atom) };
        Ok(Self {
            net_client_list: intern(b"_NET_CLIENT_LIST")?,
            net_wm_window_type: intern(b"_NET_WM_WINDOW_TYPE")?,
            net_wm_window_type_normal: intern(b"_NET_WM_WINDOW_TYPE_NORMAL")?,
            net_wm_window_type_dialog: intern(b"_NET_WM_WINDOW_TYPE_DIALOG")?,
            net_active_window: intern(b"_NET_ACTIVE_WINDOW")?,
            net_moveresize_window: intern(b"_NET_MOVERESIZE_WINDOW")?,
            net_wm_state: intern(b"_NET_WM_STATE")?,
            net_wm_state_maximized_horz: intern(b"_NET_WM_STATE_MAXIMIZED_HORZ")?,
            net_wm_state_maximized_vert: intern(b"_NET_WM_STATE_MAXIMIZED_VERT")?,
            net_wm_state_skip_taskbar: intern(b"_NET_WM_STATE_SKIP_TASKBAR")?,
            net_wm_state_skip_pager: intern(b"_NET_WM_STATE_SKIP_PAGER")?,
            net_wm_state_modal: intern(b"_NET_WM_STATE_MODAL")?,
        })
    }
}

fn fit_wechat_windows(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
    raise: bool,
    recenter_dialogs: bool,
    tracked_main: &mut Option<Window>,
    centered_dialogs: &mut HashSet<Window>,
) -> Result<()> {
    let root_geom = conn.get_geometry(root)?.reply()?;
    let prop = conn
        .get_property(
            false,
            root,
            atoms.net_client_list,
            AtomEnum::WINDOW,
            0,
            1024,
        )?
        .reply()?;
    let Some(windows_iter) = prop.value32() else {
        return Ok(());
    };
    let windows: Vec<Window> = windows_iter.collect();
    centered_dialogs.retain(|w| windows.contains(w));
    if tracked_main.is_some_and(|w| !windows.contains(&w)) {
        *tracked_main = None;
    }

    let mut candidates = Vec::new();
    for win in windows {
        let Ok(wm_class) = read_wm_class(conn, win) else {
            continue;
        };
        if !is_wechat_class(&wm_class) {
            continue;
        }
        let Ok(geom) = conn.get_geometry(win)?.reply() else {
            continue;
        };
        let Ok(primary_type) = read_primary_window_type(conn, win, atoms) else {
            continue;
        };
        let is_transient = is_transient_window(conn, win).unwrap_or(false);
        let has_auxiliary_state = has_auxiliary_wm_state(conn, win, atoms).unwrap_or(false);
        let hints = read_size_hints(conn, win).unwrap_or_default();
        candidates.push(WindowCandidate {
            window: win,
            wm_class,
            primary_type,
            is_transient,
            has_auxiliary_state,
            width: geom.width,
            height: geom.height,
            hints,
        });
    }

    let Some(selection) = select_window_to_fit(
        &candidates,
        *tracked_main,
        root_geom.width,
        root_geom.height,
    ) else {
        return Ok(());
    };

    match selection.kind {
        FitTargetKind::Main => {
            *tracked_main = Some(selection.window);
        }
        FitTargetKind::LoginDialog => {
            if !recenter_dialogs && centered_dialogs.contains(&selection.window) {
                return Ok(());
            }
            centered_dialogs.insert(selection.window);
        }
    }

    let Ok(geom) = conn.get_geometry(selection.window)?.reply() else {
        return Ok(());
    };
    let (abs_x, abs_y) = conn
        .translate_coordinates(selection.window, root, 0, 0)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|r| (r.dst_x as i32, r.dst_y as i32))
        .unwrap_or((geom.x as i32, geom.y as i32));

    if !geometry_matches(abs_x, abs_y, geom.width, geom.height, selection.geometry) {
        clear_maximized_state(conn, root, selection.window, atoms);
        moveresize_window(conn, root, selection.window, selection.geometry, atoms);
    }
    if raise {
        let _ = conn.map_window(selection.window);
        activate_window(conn, root, selection.window, atoms);
    }
    conn.flush()?;
    Ok(())
}

fn read_wm_class(conn: &RustConnection, win: Window) -> Result<String> {
    let class_reply = conn
        .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?
        .reply()?;
    Ok(String::from_utf8_lossy(&class_reply.value).into_owned())
}

fn read_primary_window_type(
    conn: &RustConnection,
    win: Window,
    atoms: &Atoms,
) -> Result<PrimaryWindowType> {
    let type_reply = conn
        .get_property(false, win, atoms.net_wm_window_type, AtomEnum::ATOM, 0, 16)?
        .reply()?;
    let Some(mut types) = type_reply.value32() else {
        return Ok(PrimaryWindowType::Normal);
    };
    match types.next() {
        None => Ok(PrimaryWindowType::Normal),
        Some(t) if t == atoms.net_wm_window_type_normal => Ok(PrimaryWindowType::Normal),
        Some(t) if t == atoms.net_wm_window_type_dialog => Ok(PrimaryWindowType::Dialog),
        Some(_) => Ok(PrimaryWindowType::Other),
    }
}

fn is_transient_window(conn: &RustConnection, win: Window) -> Result<bool> {
    let reply = conn
        .get_property(
            false,
            win,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            0,
            4,
        )?
        .reply()?;
    Ok(reply
        .value32()
        .and_then(|mut it| it.next())
        .is_some_and(|parent| parent != 0))
}

fn has_auxiliary_wm_state(conn: &RustConnection, win: Window, atoms: &Atoms) -> Result<bool> {
    let reply = conn
        .get_property(false, win, atoms.net_wm_state, AtomEnum::ATOM, 0, 32)?
        .reply()?;
    Ok(reply.value32().is_some_and(|mut states| {
        states.any(|s| {
            s == atoms.net_wm_state_skip_taskbar
                || s == atoms.net_wm_state_skip_pager
                || s == atoms.net_wm_state_modal
        })
    }))
}

fn read_size_hints(conn: &RustConnection, win: Window) -> Result<SizeHints> {
    let reply = conn
        .get_property(
            false,
            win,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::WM_SIZE_HINTS,
            0,
            18,
        )?
        .reply()?;
    let vals: Vec<u32> = reply.value32().map(|it| it.collect()).unwrap_or_default();
    if vals.len() < 9 {
        return Ok(SizeHints::default());
    }
    let flags = vals[0];
    let has_min = (flags & (1 << 4)) != 0;
    let has_max = (flags & (1 << 5)) != 0;
    Ok(SizeHints {
        min_width: has_min.then_some(vals[5]),
        min_height: has_min.then_some(vals[6]),
        max_width: has_max.then_some(vals[7]),
        max_height: has_max.then_some(vals[8]),
    })
}

fn moveresize_window(
    conn: &RustConnection,
    root: Window,
    win: Window,
    target: TargetGeometry,
    atoms: &Atoms,
) {
    // Use EWMH _NET_MOVERESIZE_WINDOW with NorthWest gravity so Openbox does not drift StaticGravity Qt frames.
    let ev = ClientMessageEvent::new(
        32,
        win,
        atoms.net_moveresize_window,
        [
            MOVERESIZE_FLAGS,
            target.x.max(0) as u32,
            target.y.max(0) as u32,
            target.width,
            target.height,
        ],
    );
    let _ = conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    );
}

fn clear_maximized_state(conn: &RustConnection, root: Window, win: Window, atoms: &Atoms) {
    let ev = ClientMessageEvent::new(
        32,
        win,
        atoms.net_wm_state,
        [
            0,
            atoms.net_wm_state_maximized_horz,
            atoms.net_wm_state_maximized_vert,
            1,
            0,
        ],
    );
    let _ = conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    );
}

fn activate_window(conn: &RustConnection, root: Window, win: Window, atoms: &Atoms) {
    let ev = ClientMessageEvent::new(32, win, atoms.net_active_window, [1, 0, 0, 0, 0]);
    let _ = conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_resizable_main_window_to_full_viewport() {
        let geom = compute_window_geometry(3816, 1744, 1908, 872, SizeHints::default());
        assert_eq!(
            geom,
            TargetGeometry {
                x: 0,
                y: 0,
                width: 3816,
                height: 1744,
            }
        );
    }

    #[test]
    fn centers_fixed_size_login_dialog_without_stretching() {
        let hints = SizeHints {
            min_width: Some(380),
            min_height: Some(500),
            max_width: Some(380),
            max_height: Some(500),
        };
        let geom = compute_window_geometry(1280, 800, 380, 500, hints);
        assert_eq!(
            geom,
            TargetGeometry {
                x: 450,
                y: 150,
                width: 380,
                height: 500,
            }
        );
    }

    #[test]
    fn geometry_matches_within_tolerance() {
        let target = TargetGeometry {
            x: 0,
            y: 0,
            width: 3816,
            height: 1744,
        };
        assert!(geometry_matches(0, 0, 3816, 1744, target));
        assert!(!geometry_matches(-133, 117, 3816, 1716, target));
    }

    fn fixed_hints(w: u32, h: u32) -> SizeHints {
        SizeHints {
            min_width: Some(w),
            min_height: Some(h),
            max_width: Some(w),
            max_height: Some(h),
        }
    }

    fn main_window(id: Window) -> WindowCandidate {
        WindowCandidate {
            window: id,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: false,
            has_auxiliary_state: false,
            width: 1000,
            height: 700,
            hints: SizeHints {
                min_width: Some(700),
                min_height: Some(500),
                max_width: None,
                max_height: None,
            },
        }
    }

    #[test]
    fn ignores_fixed_size_menus_and_search_popups_when_main_window_is_open() {
        let main = main_window(10);
        let hamburger_menu = WindowCandidate {
            window: 20,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: false,
            has_auxiliary_state: false,
            width: 180,
            height: 220,
            hints: fixed_hints(180, 220),
        };
        let search_popup = WindowCandidate {
            window: 30,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: false,
            has_auxiliary_state: false,
            width: 360,
            height: 260,
            hints: fixed_hints(360, 260),
        };

        let selected =
            select_window_to_fit(&[main, hamburger_menu, search_popup], Some(10), 1280, 800);
        assert_eq!(
            selected,
            Some(FitSelection {
                window: 10,
                kind: FitTargetKind::Main,
                geometry: TargetGeometry {
                    x: 0,
                    y: 0,
                    width: 1280,
                    height: 800,
                },
            })
        );
    }

    #[test]
    fn ignores_secondary_resizable_windows_once_main_window_exists() {
        let main = main_window(10);
        let settings_or_viewer = WindowCandidate {
            window: 40,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: false,
            has_auxiliary_state: false,
            width: 640,
            height: 520,
            hints: SizeHints::default(),
        };

        let with_tracked = select_window_to_fit(
            &[main.clone(), settings_or_viewer.clone()],
            Some(10),
            1440,
            900,
        );
        assert_eq!(with_tracked.map(|s| s.window), Some(10));

        let without_tracked = select_window_to_fit(&[main, settings_or_viewer], None, 1440, 900);
        assert_eq!(without_tracked.map(|s| s.window), Some(10));
    }

    #[test]
    fn ignores_narrow_or_short_resizable_popups() {
        let tall_search = WindowCandidate {
            window: 21,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: false,
            has_auxiliary_state: false,
            width: 360,
            height: 520,
            hints: SizeHints::default(),
        };
        let wide_bar = WindowCandidate {
            window: 22,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: false,
            has_auxiliary_state: false,
            width: 600,
            height: 240,
            hints: SizeHints::default(),
        };

        assert_eq!(
            select_window_to_fit(&[tall_search, wide_bar], None, 1280, 800),
            None
        );
    }

    #[test]
    fn centers_standalone_login_dialog_when_no_main_window_exists() {
        let login = WindowCandidate {
            window: 5,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Dialog,
            is_transient: false,
            has_auxiliary_state: false,
            width: 380,
            height: 500,
            hints: fixed_hints(380, 500),
        };
        let small_menu = WindowCandidate {
            window: 6,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: false,
            has_auxiliary_state: false,
            width: 160,
            height: 120,
            hints: fixed_hints(160, 120),
        };

        let selected = select_window_to_fit(&[login, small_menu], None, 1280, 800);
        assert_eq!(
            selected,
            Some(FitSelection {
                window: 5,
                kind: FitTargetKind::LoginDialog,
                geometry: TargetGeometry {
                    x: 450,
                    y: 150,
                    width: 380,
                    height: 500,
                },
            })
        );
    }

    #[test]
    fn ignores_transient_and_auxiliary_fixed_windows_without_main_window() {
        let transient_popup = WindowCandidate {
            window: 7,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: true,
            has_auxiliary_state: false,
            width: 380,
            height: 500,
            hints: fixed_hints(380, 500),
        };
        let skip_taskbar_popup = WindowCandidate {
            window: 8,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Normal,
            is_transient: false,
            has_auxiliary_state: true,
            width: 380,
            height: 500,
            hints: fixed_hints(380, 500),
        };
        let menu_type_popup = WindowCandidate {
            window: 9,
            wm_class: "wechat\0wechat".to_string(),
            primary_type: PrimaryWindowType::Other,
            is_transient: false,
            has_auxiliary_state: false,
            width: 380,
            height: 500,
            hints: fixed_hints(380, 500),
        };

        assert_eq!(
            select_window_to_fit(
                &[transient_popup, skip_taskbar_popup, menu_type_popup],
                None,
                1280,
                800
            ),
            None
        );
    }
}
