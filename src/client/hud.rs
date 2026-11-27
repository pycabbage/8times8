use std::fmt::Display;

use _8times8::{Amount, Game, Level};
use winit::dpi::PhysicalSize;

use super::{Painter, Theme};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Shop,
    Purchase,
    MachineBody(usize),
    MachineUpgrade(usize, usize),
    Debug,
    DebugCommand(DebugAction),
    DebugConfirm,
    DebugCancel,
    Sound,
    Theme,
    ZoomOut,
    ZoomIn,
    Home,
    Block,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DebugAction {
    Reset,
    JumpItem,
    JumpBig,
    Grant(Level, u128),
    ClearBonus,
    ManualMax,
    ManualZero,
    Pause,
    Speed,
    ItemGet,
    ItemReset,
    CopySave,
    PasteSave,
}

#[derive(Debug)]
pub(super) struct MachineTier {
    pub(super) tier: usize,
    pub(super) owned: usize,
    pub(super) limit: usize,
    pub(super) legacy: usize,
    pub(super) body: Option<MachineRow>,
    pub(super) machines: Vec<MachineRow>,
}

#[derive(Debug)]
pub(super) struct MachineRow {
    pub(super) id: usize,
    pub(super) current: String,
    pub(super) next: Option<String>,
    pub(super) cost: String,
    pub(super) enabled: bool,
}

struct Stock {
    level: Level,
    available: String,
    cleared: String,
    reached: bool,
}

#[derive(Clone)]
struct Purchase {
    name: String,
    description: String,
    caption: String,
    enabled: bool,
    action: Action,
}

pub(super) struct Hud {
    stock: Vec<Stock>,
    manual: Option<Purchase>,
    manual_capacity: String,
    item: bool,
    item_on_board: bool,
    tiers: Vec<MachineTier>,
    shop_lines: Vec<ShopLine>,
    shop_height: f64,
    shop_open: bool,
    shop_scroll: f64,
    debug_open: bool,
    debug_scroll: f64,
    debug_pending: Option<DebugAction>,
    debug_paused: bool,
    debug_speed: u32,
    debug_feedback: Option<String>,
    sound: Option<bool>,
    hint: bool,
    save_blocked: bool,
    focus: Option<Action>,
    tooltip: Option<Action>,
}

impl Default for Hud {
    fn default() -> Self {
        Self {
            stock: Vec::new(),
            manual: None,
            manual_capacity: capacity_label(0),
            item: false,
            item_on_board: false,
            tiers: Vec::new(),
            shop_lines: Vec::new(),
            shop_height: 0.0,
            shop_open: false,
            shop_scroll: 0.0,
            debug_open: false,
            debug_scroll: 0.0,
            debug_pending: None,
            debug_paused: false,
            debug_speed: 1,
            debug_feedback: None,
            sound: None,
            hint: true,
            save_blocked: false,
            focus: None,
            tooltip: None,
        }
    }
}

impl Hud {
    /// Keep the drawn labels and pointer/keyboard targets on the same game snapshot.
    pub(super) fn update(&mut self, game: &Game) {
        self.stock = Level::ALL
            .into_iter()
            .map(|level| {
                let available = game.available(level);
                let cleared = game.cleared(level);
                Stock {
                    level,
                    reached: level == Level::A
                        || cleared.bits() > 0
                        || game.bonus(level).bits() > 0,
                    available: points_label(available, level),
                    cleared: points_label(cleared, level),
                }
            })
            .collect();
        self.manual_capacity = capacity_label(game.manual_power().trailing_zeros());
        self.manual =
            game.next_upgrade()
                .zip(game.next_manual_power())
                .map(|((level, cost), next)| {
                    let next = capacity_label(next.trailing_zeros());
                    Purchase {
                        name: "1回で返す量".into(),
                        description: format!("現在：{}／回 → 次：{next}／回", self.manual_capacity),
                        caption: format!("強化 {}", points_label(cost, level)),
                        enabled: game.available(level).covers(&Amount::from(cost)),
                        action: Action::Purchase,
                    }
                });
        self.item = game.has_drag_item();
        self.item_on_board = game.item().is_some();
        self.rebuild_shop();
    }

    pub(super) fn set_machine_rows(&mut self, tiers: Vec<MachineTier>) {
        self.tiers = tiers;
        self.rebuild_shop();
    }

    pub(super) fn set_sound(&mut self, sound: Option<bool>) {
        self.sound = sound;
    }

    pub(super) fn set_hint(&mut self, hint: bool) {
        self.hint = hint;
    }

    pub(super) fn set_save_blocked(&mut self, blocked: bool) {
        self.save_blocked = blocked;
    }

    pub(super) fn set_tooltip(&mut self, action: Option<Action>) {
        self.tooltip = action;
    }

    pub(super) fn tooltip_action(
        &self,
        size: PhysicalSize<u32>,
        scale: f64,
        pos: [f64; 2],
    ) -> Option<Action> {
        let layout = self.layout(size, scale)?;
        self.tooltip_at(&layout, layout.logical(pos))
    }

    fn tooltip_at(&self, layout: &Layout<'_>, pos: [f64; 2]) -> Option<Action> {
        if self.debug_pending.is_some() {
            return None;
        }
        if layout.item.is_some_and(|r| r.contains(pos)) {
            return Some(Action::Block);
        }
        layout
            .controls
            .iter()
            .find_map(|&(action, rect)| rect.contains(pos).then_some(action))
    }

    pub(super) fn toggle_shop(&mut self) {
        if self.debug_pending.is_some() {
            return;
        }
        self.shop_open = !self.shop_open;
        if self.shop_open {
            self.shop_scroll = 0.0;
            self.debug_open = false;
        }
    }

    pub(super) fn toggle_debug(&mut self) {
        self.debug_open = !self.debug_open;
        if self.debug_open {
            self.debug_scroll = 0.0;
            self.debug_feedback = None;
            self.shop_open = false;
        } else {
            self.set_debug_pending(None);
        }
    }

    pub(super) fn set_debug_pending(&mut self, command: Option<DebugAction>) {
        if command.is_some() {
            self.debug_open = true;
            self.shop_open = false;
            self.focus = Some(Action::DebugCancel);
        } else if matches!(self.focus, Some(Action::DebugConfirm | Action::DebugCancel)) {
            self.focus = None;
        }
        self.debug_pending = command;
    }

    pub(super) fn set_debug_machine_state(&mut self, paused: bool, speed: u32) {
        self.debug_paused = paused;
        self.debug_speed = speed;
    }

    pub(super) fn set_debug_feedback(&mut self, feedback: Option<String>) {
        self.debug_feedback = feedback;
    }

    pub(super) fn set_focus(&mut self, action: Option<Action>) {
        self.focus = action;
    }

    pub(super) fn focused_action(&self) -> Option<Action> {
        let action = self.focus?;
        if self.debug_pending.is_some() {
            return matches!(action, Action::DebugConfirm | Action::DebugCancel).then_some(action);
        }
        match action {
            Action::Purchase | Action::MachineBody(_) | Action::MachineUpgrade(_, _) => (self
                .shop_open
                && self
                    .enabled_purchases()
                    .any(|(_, row)| row.action == action))
            .then_some(action),
            Action::DebugCommand(_) => self.debug_open.then_some(action),
            Action::DebugConfirm | Action::DebugCancel => None,
            Action::Sound => self.sound.map(|_| action),
            Action::Block => None,
            _ => Some(action),
        }
    }

    pub(super) fn tab(
        &mut self,
        size: PhysicalSize<u32>,
        scale_factor: f64,
        backwards: bool,
    ) -> Option<Action> {
        let layout = self.layout(size, scale_factor)?;
        let mut items = Vec::new();
        if self.debug_pending.is_none() {
            items.push((Action::Shop, None));
            if let Some(view) = layout.shop {
                items.extend(
                    self.enabled_purchases().map(|(y, row)| {
                        (row.action, Some((view, view.button_rect(y, &row.caption))))
                    }),
                );
            }
            items.push((Action::Debug, None));
        }
        if let Some(debug) = &layout.debug {
            items.extend(
                debug
                    .buttons()
                    .map(|(a, _, rect)| (a, Some((debug.view, rect)))),
            );
        }
        if self.debug_pending.is_none() {
            items.extend(layout.controls.iter().map(|&(a, _)| (a, None)));
        }
        if items.is_empty() {
            return None;
        }
        let i = match items.iter().position(|&(a, _)| Some(a) == self.focus) {
            Some(0) | None if backwards => items.len() - 1,
            Some(i) if backwards => i - 1,
            Some(i) => (i + 1) % items.len(),
            None => 0,
        };
        let (action, target) = items[i];
        if let Some((view, rect)) = target {
            let scroll = view.reveal(rect);
            if matches!(action, Action::DebugCommand(_)) {
                self.debug_scroll = scroll;
            } else if matches!(
                action,
                Action::Purchase | Action::MachineBody(_) | Action::MachineUpgrade(_, _)
            ) {
                self.shop_scroll = scroll;
            }
        }
        self.focus = Some(action);
        self.focus
    }

    /// Returns true whenever the HUD, rather than the board, owns this wheel event.
    pub(super) fn scroll(
        &mut self,
        size: PhysicalSize<u32>,
        scale_factor: f64,
        pos: [f64; 2],
        logical_delta_y: f64,
    ) -> bool {
        let Some(layout) = self.layout(size, scale_factor) else {
            return false;
        };
        let pos = layout.logical(pos);
        if let Some(view) = layout.shop
            && view.rect.contains(pos)
        {
            if logical_delta_y.is_finite() {
                self.shop_scroll = (view.scroll - logical_delta_y).clamp(0.0, view.max_scroll);
            }
            return true;
        }
        if let Some(debug) = &layout.debug
            && debug.view.rect.contains(pos)
        {
            if self.debug_pending.is_none() && logical_delta_y.is_finite() {
                self.debug_scroll =
                    (debug.view.scroll - logical_delta_y).clamp(0.0, debug.view.max_scroll);
            }
            return true;
        }
        self.hit_layout(&layout, pos).is_some()
    }

    pub(super) fn hit(
        &self,
        size: PhysicalSize<u32>,
        scale_factor: f64,
        pos: [f64; 2],
    ) -> Option<Action> {
        let layout = self.layout(size, scale_factor)?;
        self.hit_layout(&layout, layout.logical(pos))
    }

    fn hit_layout(&self, layout: &Layout, pos: [f64; 2]) -> Option<Action> {
        if let Some(debug) = &layout.debug
            && debug.view.rect.contains(pos)
        {
            return Some(
                debug
                    .buttons()
                    .find_map(|(action, _, rect)| {
                        (debug.view.clip.fits(rect) && rect.contains(pos)).then_some(action)
                    })
                    .unwrap_or(Action::Block),
            );
        }
        if self.debug_pending.is_some() {
            return Some(Action::Block);
        }
        if layout.debug_button.contains(pos) {
            return Some(Action::Debug);
        }
        if layout.shop_button.contains(pos) {
            return Some(Action::Shop);
        }
        if layout.status.contains(pos) {
            return Some(Action::Block);
        }
        if let Some(view) = layout.shop
            && view.rect.contains(pos)
        {
            for line in &self.shop_lines {
                if let ShopLine::Row(y, row) = line
                    && row.enabled
                    && view
                        .button(*y, &row.caption)
                        .is_some_and(|r| r.contains(pos))
                {
                    return Some(row.action);
                }
            }
            return Some(Action::Block);
        }
        for &(action, rect) in &layout.controls {
            if rect.contains(pos) {
                return Some(action);
            }
        }
        let (_, first) = layout.controls.first()?;
        let (_, last) = layout.controls.last()?;
        Rect::new(first.x, first.y, last.right() - first.x, first.h)
            .contains(pos)
            .then_some(Action::Block)
    }

    pub(super) fn paint(
        &self,
        painter: &mut Painter<'_>,
        _game: &Game,
        theme: Theme,
        scale_factor: f64,
        cursor: Option<[f64; 2]>,
    ) {
        let Some(layout) = self.layout(painter.size(), scale_factor) else {
            return;
        };
        let colors = Palette::for_theme(theme);
        let pointer = cursor.map(|pos| layout.logical(pos));
        let hover = pointer.and_then(|pos| self.hit_layout(&layout, pos));
        let mut draw = Draw {
            painter,
            scale: layout.scale,
            colors,
            hover,
            focus: self.focused_action(),
        };

        if self.hint {
            paint_hint(&mut draw, &layout);
        }

        draw.round(
            Rect::new(
                layout.status.x,
                layout.status.y + 1.0,
                layout.status.w,
                layout.status.h + 2.0,
            ),
            12.0,
            colors.shadow,
        );
        draw.round(layout.status, 12.0, colors.status);
        for (text, x, y) in &layout.points {
            draw.text_tracked(
                text,
                *x,
                *y,
                15.0,
                colors.points_ink,
                colors.status,
                Some(layout.status),
                0.9,
            );
        }
        if let Some(icon) = layout.item {
            draw.item(icon);
        }
        draw.label_button(layout.shop_button, SHOP_TITLE, Action::Shop, true);
        if let Some(rect) = layout.save_warning {
            draw.round(rect, 6.0, colors.warning);
            draw.text(
                SAVE_WARNING,
                rect.x + 6.0,
                rect.y + 3.0,
                12.0,
                colors.warning_ink,
                colors.warning,
                Some(rect),
            );
        }

        if let Some(view) = layout.shop {
            self.paint_shop(&mut draw, view);
        }
        if let Some(debug) = &layout.debug {
            paint_debug(&mut draw, debug, self.debug_feedback.as_deref());
        }
        draw.label_button(layout.debug_button, DEBUG_TITLE, Action::Debug, true);

        for &(action, rect) in &layout.controls {
            let bg = draw.button(rect, action, true, 10.0);
            draw.icon(rect, action, theme, self.sound == Some(true), bg);
        }
        if let (Some(action), Some(pos)) = (self.tooltip, pointer)
            && self.tooltip_at(&layout, pos) == Some(action)
        {
            paint_tooltip(&mut draw, &layout, action, pos);
        }
    }

    fn rebuild_shop(&mut self) {
        let mut lines = Vec::new();
        let mut y = 0.0;
        if let Some(manual) = &self.manual {
            lines.push(ShopLine::Heading(y, "手動".into(), 0));
            y += SHOP_HEADING_HEIGHT;
            lines.push(ShopLine::Row(y, manual.clone()));
            y += ROW_HEIGHT;
        }
        for tier in &self.tiers {
            let Some(&level) = tier.tier.checked_sub(1).and_then(|i| Level::ALL.get(i)) else {
                continue;
            };
            let letter = letter(level);
            if y > 0.0 {
                lines.push(ShopLine::Divider(y + 10.0));
                y += 19.0;
            }
            lines.push(ShopLine::Heading(y, format!("{letter}機械"), tier.tier));
            y += SHOP_HEADING_HEIGHT;
            let legacy = if tier.legacy > 0 {
                format!("（移行分 +{}台）", tier.legacy)
            } else {
                String::new()
            };
            let balance = self
                .stock
                .iter()
                .find(|s| s.level == level)
                .map_or_else(|| format!("0{letter}"), |s| s.available.clone());
            lines.push(ShopLine::Meta(
                y,
                format!(
                    "所有 {}/{}台{legacy}　所持 {balance}",
                    tier.owned, tier.limit
                ),
            ));
            y += 21.0;
            if let Some(body) = &tier.body
                && tier.owned < tier.limit
            {
                lines.push(ShopLine::Row(
                    y,
                    Purchase {
                        name: "本体".into(),
                        description: format!("{}／台　購入時 {}／動作", body.cost, body.current),
                        caption: format!("購入 {}", body.cost),
                        enabled: body.enabled,
                        action: Action::MachineBody(tier.tier),
                    },
                ));
                y += ROW_HEIGHT;
            }
            for (i, machine) in tier.machines.iter().enumerate() {
                let mut description = format!("現在：{}／動作", machine.current);
                if let Some(next) = &machine.next {
                    description.push_str(&format!(" → 次：{next}／動作"));
                }
                lines.push(ShopLine::Row(
                    y,
                    Purchase {
                        name: format!("{letter}機械 {}号", i + 1),
                        description,
                        caption: format!("強化 {}", machine.cost),
                        enabled: machine.enabled,
                        action: Action::MachineUpgrade(tier.tier, machine.id),
                    },
                ));
                y += ROW_HEIGHT;
            }
        }
        self.shop_lines = lines;
        self.shop_height = y;
    }

    fn enabled_purchases(&self) -> impl Iterator<Item = (f64, &Purchase)> + '_ {
        self.shop_lines.iter().filter_map(|line| match line {
            ShopLine::Row(y, row) if row.enabled => Some((*y, row)),
            _ => None,
        })
    }

    fn layout(&self, size: PhysicalSize<u32>, scale: f64) -> Option<Layout<'_>> {
        if !scale.is_finite() || scale <= 0.0 || size.width == 0 || size.height == 0 {
            return None;
        }
        let w = f64::from(size.width) / scale;
        let h = f64::from(size.height) / scale;
        if w < 80.0 || h < 80.0 {
            return None;
        }

        let mut points = Vec::new();
        let (mut px, mut py, mut right) = (24.0, 20.0, 24.0_f64);
        // Keep balances clear of the top-right debug button in a narrow window.
        let point_limit = (w - 97.0).max(80.0);
        for label in self
            .stock
            .iter()
            .rev()
            .filter(|s| s.reached)
            .map(|s| s.available.as_str())
        {
            let width = tracked_width(label, 15.0, 0.9);
            if px > 24.0 && px + width > point_limit {
                px = 24.0;
                py += STATUS_ROW_HEIGHT;
            }
            points.push((label, px, py + 3.0));
            right = right.max(px + width);
            px += width + 15.0;
        }
        if points.is_empty() {
            points.push(("0A", px, py + 3.0));
            px += tracked_width("0A", 15.0, 0.9) + 15.0;
        }
        let item = self.item.then(|| {
            px -= 7.0;
            if px + 20.0 > point_limit {
                px = 24.0;
                py += STATUS_ROW_HEIGHT;
            }
            right = right.max(px + 20.0);
            Rect::new(px, py + 0.5, 20.0, 20.0)
        });
        let shop_button = Rect::new(
            24.0,
            py + STATUS_ROW_HEIGHT + 7.0,
            title_width(SHOP_TITLE).min(w - 40.0),
            29.0,
        );
        let save_warning = self.save_blocked.then(|| {
            Rect::new(
                24.0,
                shop_button.bottom() + 6.0,
                (text_width(SAVE_WARNING, 12.0) + 12.0).min(w - 40.0),
                20.0,
            )
        });
        let bottom = save_warning.map_or_else(
            || shop_button.bottom(),
            |rect| {
                right = right.max(rect.right());
                rect.bottom()
            },
        );
        let status = Rect::new(
            14.0,
            12.0,
            right.max(shop_button.right()) - 4.0,
            bottom + 9.0 - 12.0,
        );
        let shop_top = 92.0_f64.max(status.bottom() + 6.0);
        let shop_w = 340.0_f64.min(w - 32.0);
        let shop_h = (self.shop_height + 20.0).min(h - shop_top - 78.0);
        let shop = (self.shop_open && shop_h >= 42.0).then(|| {
            let rect = Rect::new(16.0, shop_top, shop_w, shop_h);
            let clip = Rect::new(rect.x + 12.0, rect.y + 10.0, rect.w - 24.0, rect.h - 20.0);
            let max_scroll = (self.shop_height - clip.h).max(0.0);
            ShopView {
                rect,
                clip,
                scroll: self.shop_scroll.min(max_scroll),
                max_scroll,
            }
        });

        let debug_width = title_width(DEBUG_TITLE);
        let debug_button = Rect::new(w - 16.0 - debug_width, 14.0, debug_width, 29.0);
        let debug = (self.debug_open && w >= 240.0 && h >= 175.0).then(|| self.debug_view(w, h));

        let actions = [
            Action::Sound,
            Action::Theme,
            Action::ZoomOut,
            Action::ZoomIn,
            Action::Home,
        ];
        let actions = if self.sound.is_some() {
            &actions[..]
        } else {
            &actions[1..]
        };
        let count = actions.len() as f64;
        let side = 36.0_f64.min((w - 32.0 - (count - 1.0) * 6.0) / count);
        let left = w - 16.0 - count * side - (count - 1.0) * 6.0;
        let controls = actions
            .iter()
            .enumerate()
            .map(|(i, &action)| {
                (
                    action,
                    Rect::new(left + i as f64 * (side + 6.0), h - 16.0 - side, side, side),
                )
            })
            .collect();
        Some(Layout {
            scale,
            w,
            h,
            status,
            points,
            item,
            shop_button,
            save_warning,
            shop,
            debug_button,
            debug,
            controls,
        })
    }

    fn paint_shop(&self, draw: &mut Draw<'_, '_>, view: ShopView) {
        let colors = draw.colors;
        draw.round(view.rect, 12.0, colors.panel);
        let clip = Some(view.clip);
        for line in &self.shop_lines {
            match line {
                ShopLine::Divider(y) => {
                    let y = view.y(*y);
                    if let Some(rule) =
                        view.clip
                            .intersection(Rect::new(view.clip.x, y, view.clip.w, 1.0))
                    {
                        draw.rect(rule, colors.rule);
                    }
                }
                ShopLine::Heading(y, text, dots) => {
                    let (x, y) = (view.clip.x, view.y(*y) + 3.0);
                    draw.text(text, x, y, 13.0, colors.ink, colors.panel, clip);
                    let x = x + text_width(text, 13.0) + 6.0;
                    for i in 0..*dots {
                        let dot = Rect::new(x + i as f64 * 6.0, y + 5.0, 4.0, 4.0);
                        if view.clip.fits(dot) {
                            draw.round(dot, 2.0, colors.muted);
                        }
                    }
                }
                ShopLine::Meta(y, text) => draw.text(
                    text,
                    view.clip.x,
                    view.y(*y) + 3.0,
                    12.0,
                    colors.muted,
                    colors.panel,
                    clip,
                ),
                ShopLine::Row(y, row) => {
                    let top = view.y(*y);
                    if top >= view.clip.bottom() || top + ROW_HEIGHT <= view.clip.y {
                        continue;
                    }
                    let button = view.button_rect(*y, &row.caption);
                    let text_clip = Some(Rect::new(
                        view.clip.x,
                        view.clip.y,
                        button.x - view.clip.x - 8.0,
                        view.clip.h,
                    ));
                    draw.text(
                        &row.name,
                        view.clip.x,
                        top + 8.0,
                        12.0,
                        colors.ink,
                        colors.panel,
                        text_clip,
                    );
                    draw.text(
                        &row.description,
                        view.clip.x,
                        top + 26.0,
                        12.0,
                        colors.muted,
                        colors.panel,
                        text_clip,
                    );
                    if view.clip.fits(button) {
                        draw.label_button(button, &row.caption, row.action, row.enabled);
                    }
                }
            }
        }
        draw_scrollbar(draw, view);
    }

    fn debug_view(&self, width: f64, height: f64) -> DebugView {
        use DebugAction::*;
        let w = 330.0_f64.min(width - 32.0);
        let mut menu = DebugBuilder::new(w - 24.0);
        if let Some(pending) = self.debug_pending {
            let question = match pending {
                Reset => "進行状況をリセットしますか？",
                JumpItem => "アイテム取得の直前まで進めますか？",
                JumpBig => "最初の1Dの完成直後まで進めますか？",
                PasteSave => "進行状況をこのセーブで置き換えますか？",
                _ => "実行しますか？",
            };
            menu.section("確認（デバッグ用）");
            menu.add_label(question, 0.0, false);
            menu.y += 20.0;
            menu.buttons(&[
                (Action::DebugCancel, "キャンセル"),
                (Action::DebugConfirm, "実行"),
            ]);
        } else {
            menu.section("進行");
            menu.commands(&[
                (Reset, "リセット"),
                (JumpItem, "アイテム直前"),
                (JumpBig, "1D完成直後"),
            ]);
            menu.section("ポイント（デバッグ用に別枠で加算・保存）");
            for level in Level::ALL.into_iter().take(5) {
                menu.x = 16.0;
                menu.add_label(letter(level).to_string(), 0.0, true);
                for (gain, text) in [(100, "+100"), (10_000, "+1万"), (100_000_000, "+1億")] {
                    menu.button(Action::DebugCommand(Grant(level, gain)), text);
                }
                if let Some(stock) = self.stock.iter().find(|s| s.level == level) {
                    let x = (menu.width - text_width(&stock.available, 12.0)).max(menu.x);
                    menu.add_label(&stock.available, x, false);
                }
                menu.finish_row();
            }
            menu.commands(&[(ClearBonus, "加算分を消す")]);
            menu.section("手動");
            let manual = format!("{}／回", self.manual_capacity);
            menu.add_label(&manual, 0.0, false);
            menu.x = text_width(&manual, 12.0) + 9.0;
            menu.commands(&[(ManualMax, "最大にする"), (ManualZero, "強化なしに戻す")]);
            let count: usize = self.tiers.iter().map(|t| t.machines.len()).sum();
            menu.section(&format!("機械（{count}台）"));
            let pause = if self.debug_paused {
                "再開する"
            } else {
                "一時停止"
            };
            let next_speed = if self.debug_speed == 1 { 10 } else { 1 };
            menu.commands(&[
                (Pause, pause),
                (
                    Speed,
                    &format!("速さ ×{} → ×{next_speed}", self.debug_speed),
                ),
            ]);
            let item = if self.item {
                "入手済み"
            } else if self.item_on_board {
                "盤面で待機中"
            } else {
                "未出現"
            };
            menu.section(&format!("アイテム（{item}）"));
            menu.commands(&[(ItemGet, "入手済みにする"), (ItemReset, "未出現に戻す")]);
            menu.section("セーブ");
            menu.commands(&[(CopySave, "コピー"), (PasteSave, "貼り付け")]);
            menu.section("状態");
            let complete: Vec<_> = self
                .stock
                .iter()
                .filter(|s| s.reached)
                .map(|s| s.cleared.as_str())
                .collect();
            menu.add_label(format!("完成区画 {}", complete.join(" ")), 0.0, false);
            menu.y += 17.0;
        }
        let footer =
            self.debug_pending.is_none() && self.debug_feedback.is_some() && height >= 200.0;
        let footer_height = if footer { 21.0 } else { 0.0 };
        let h = (menu.y + 18.0 + footer_height).min(if self.debug_pending.is_some() {
            height - 60.0
        } else {
            height - 130.0
        });
        let rect = Rect::new(width - 16.0 - w, 50.0, w, h);
        let clip = Rect::new(
            rect.x + 12.0,
            rect.y + 9.0,
            rect.w - 24.0,
            rect.h - 18.0 - footer_height,
        );
        let max_scroll = (menu.y - clip.h).max(0.0);
        let scroll = if self.debug_pending.is_some() {
            0.0
        } else {
            self.debug_scroll.min(max_scroll)
        };
        DebugView {
            view: ShopView {
                rect,
                clip,
                scroll,
                max_scroll,
            },
            parts: menu.parts,
            footer,
        }
    }
}

fn draw_scrollbar(draw: &mut Draw<'_, '_>, view: ShopView) {
    if view.max_scroll > 0.0 {
        let track = view.clip.h - 2.0;
        let thumb = (track * view.clip.h / (view.clip.h + view.max_scroll)).clamp(8.0, track);
        let y = view.clip.y + 1.0 + (track - thumb) * view.scroll / view.max_scroll;
        draw.round(
            Rect::new(view.rect.right() - 6.0, y, 2.0, thumb),
            1.0,
            draw.colors.rule,
        );
    }
}

fn paint_debug(draw: &mut Draw<'_, '_>, debug: &DebugView, feedback: Option<&str>) {
    let colors = draw.colors;
    let view = debug.view;
    draw.round(view.rect, 12.0, colors.panel);
    for part in &debug.parts {
        match part {
            DebugPart::Divider(y) => {
                if let Some(r) =
                    view.clip
                        .intersection(Rect::new(view.clip.x, view.y(*y), view.clip.w, 1.0))
                {
                    draw.rect(r, colors.rule);
                }
            }
            DebugPart::Label(x, y, label, ink) => {
                let fg = if *ink { colors.ink } else { colors.muted };
                draw.text(
                    label,
                    view.clip.x + x,
                    view.y(*y),
                    12.0,
                    fg,
                    colors.panel,
                    Some(view.clip),
                );
            }
            DebugPart::Button(action, r, label) => {
                let r = view.place(*r);
                if view.clip.fits(r) {
                    draw.label_button(r, label, *action, true);
                }
            }
        }
    }
    draw_scrollbar(draw, view);
    if debug.footer
        && let Some(text) = feedback
    {
        let y = view.clip.bottom() + 4.0;
        draw.rect(Rect::new(view.clip.x, y, view.clip.w, 1.0), colors.rule);
        let footer = Rect::new(view.clip.x, y + 4.0, view.clip.w, 16.0);
        draw.text(
            text,
            footer.x,
            footer.y,
            12.0,
            colors.ink,
            colors.panel,
            Some(footer),
        );
    }
}

fn letter(level: Level) -> char {
    (b'A' + level as u8).into()
}

pub(super) fn points_label(value: impl Display, level: Level) -> String {
    let digits = value.to_string();
    let n = digits.len();
    let mut label = String::new();
    let comma = |s: &str, out: &mut String| {
        for (i, ch) in s.chars().enumerate() {
            if i > 0 && (s.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(ch);
        }
    };
    if n < 10 {
        comma(&digits, &mut label);
    } else if n <= 24 {
        let unit = (n - 1 - 8) / 4;
        let whole = n - 8 - 4 * unit;
        comma(&digits[..whole], &mut label);
        let fraction = digits[whole..whole + 2].trim_end_matches('0');
        if !fraction.is_empty() {
            label.push('.');
            label.push_str(fraction);
        }
        label.push(['億', '兆', '京', '垓'][unit]);
    } else {
        label = format!("{}.{}e{}", &digits[..1], &digits[1..3], n - 1);
    }
    label.push(letter(level));
    label
}

pub(super) fn capacity_label(exponent: u32) -> String {
    if exponent < 6 {
        return format!("{}枚", 1 << exponent);
    }
    match Level::ALL.get(exponent as usize / 6 - 1) {
        Some(level) => format!("{}{}", 1 << (exponent % 6), letter(*level)),
        None => format!("2^{exponent}枚"),
    }
}

const SHOP_TITLE: &str = "強化・機械";
const DEBUG_TITLE: &str = "デバッグ";
const SAVE_WARNING: &str = "自動保存停止中";
const STATUS_ROW_HEIGHT: f64 = 21.0;
const SHOP_HEADING_HEIGHT: f64 = 21.0;
const ROW_HEIGHT: f64 = 45.0;
const ROW_BUTTON_HEIGHT: f64 = 25.0;

fn title_width(label: &str) -> f64 {
    tracked_width(label, 12.0, 0.48) + 20.0
}

enum ShopLine {
    Divider(f64),
    Heading(f64, String, usize),
    Meta(f64, String),
    Row(f64, Purchase),
}

enum DebugPart {
    Divider(f64),
    Label(f64, f64, String, bool),
    Button(Action, Rect, String),
}

struct DebugBuilder {
    x: f64,
    y: f64,
    width: f64,
    parts: Vec<DebugPart>,
}

impl DebugBuilder {
    fn new(width: f64) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width,
            parts: Vec::new(),
        }
    }

    fn section(&mut self, name: &str) {
        if self.y > 0.0 {
            self.parts.push(DebugPart::Divider(self.y + 2.0));
            self.y += 8.0;
        }
        self.add_label(name, 0.0, true);
        self.y += 16.0;
    }

    fn add_label(&mut self, text: impl Into<String>, x: f64, ink: bool) {
        self.parts
            .push(DebugPart::Label(x, self.y + 3.0, text.into(), ink));
    }

    fn button(&mut self, action: Action, text: &str) {
        let w = (text_width(text, 12.0) + 15.0).max(36.0).min(self.width);
        if self.x > 0.0 && self.x + w > self.width {
            self.finish_row();
        }
        let rect = Rect::new(self.x, self.y, w, 21.0);
        self.parts
            .push(DebugPart::Button(action, rect, text.into()));
        self.x += w + 4.0;
    }

    fn commands(&mut self, items: &[(DebugAction, &str)]) {
        for &(action, caption) in items {
            self.button(Action::DebugCommand(action), caption);
        }
        self.finish_row();
    }

    fn buttons(&mut self, items: &[(Action, &str)]) {
        for &(action, caption) in items {
            self.button(action, caption);
        }
        self.finish_row();
    }

    fn finish_row(&mut self) {
        self.y += 25.0;
        self.x = 0.0;
    }
}

struct DebugView {
    view: ShopView,
    parts: Vec<DebugPart>,
    footer: bool,
}

impl DebugView {
    fn buttons(&self) -> impl Iterator<Item = (Action, &str, Rect)> {
        self.parts.iter().filter_map(|part| match part {
            DebugPart::Button(action, rect, text) => {
                Some((*action, text.as_str(), self.view.place(*rect)))
            }
            _ => None,
        })
    }
}

struct Layout<'a> {
    scale: f64,
    w: f64,
    h: f64,
    status: Rect,
    points: Vec<(&'a str, f64, f64)>,
    item: Option<Rect>,
    shop_button: Rect,
    save_warning: Option<Rect>,
    shop: Option<ShopView>,
    debug_button: Rect,
    debug: Option<DebugView>,
    controls: Vec<(Action, Rect)>,
}

impl Layout<'_> {
    fn logical(&self, [x, y]: [f64; 2]) -> [f64; 2] {
        [x / self.scale, y / self.scale]
    }
}

#[derive(Clone, Copy)]
struct ShopView {
    rect: Rect,
    clip: Rect,
    scroll: f64,
    max_scroll: f64,
}

impl ShopView {
    fn y(self, content_y: f64) -> f64 {
        self.clip.y + content_y - self.scroll
    }

    fn place(self, content_rect: Rect) -> Rect {
        Rect::new(
            self.clip.x + content_rect.x,
            self.y(content_rect.y),
            content_rect.w,
            content_rect.h,
        )
    }

    fn reveal(self, rect: Rect) -> f64 {
        (self.scroll
            + (rect.y - self.clip.y).min(0.0)
            + (rect.bottom() - self.clip.bottom()).max(0.0))
        .clamp(0.0, self.max_scroll)
    }

    fn button_rect(self, y: f64, caption: &str) -> Rect {
        let w = (text_width(caption, 12.0) + 18.0)
            .clamp(56.0, 126.0)
            .min(self.clip.w * 0.47);
        Rect::new(
            self.clip.right() - w,
            self.y(y) + (ROW_HEIGHT - ROW_BUTTON_HEIGHT) / 2.0,
            w,
            ROW_BUTTON_HEIGHT,
        )
    }

    fn button(self, y: f64, caption: &str) -> Option<Rect> {
        let rect = self.button_rect(y, caption);
        self.clip.fits(rect).then_some(rect)
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Rect {
    fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    fn right(self) -> f64 {
        self.x + self.w
    }
    fn bottom(self) -> f64 {
        self.y + self.h
    }
    fn contains(self, [x, y]: [f64; 2]) -> bool {
        self.x <= x && x < self.right() && self.y <= y && y < self.bottom()
    }
    fn fits(self, other: Self) -> bool {
        let eps = 1e-8;
        self.x <= other.x + eps
            && other.right() <= self.right() + eps
            && self.y <= other.y + eps
            && other.bottom() <= self.bottom() + eps
    }
    fn intersection(self, other: Self) -> Option<Self> {
        let (x, y) = (self.x.max(other.x), self.y.max(other.y));
        let (w, h) = (
            self.right().min(other.right()) - x,
            self.bottom().min(other.bottom()) - y,
        );
        (w > 0.0 && h > 0.0).then_some(Self::new(x, y, w, h))
    }
}

type Color = [u8; 3];

#[derive(Clone, Copy)]
struct Palette {
    status: Color,
    shadow: Color,
    panel: Color,
    rule: Color,
    ink: Color,
    points_ink: Color,
    title_ink: Color,
    debug_ink: Color,
    muted: Color,
    button: Color,
    hover: Color,
    title: Color,
    title_hover: Color,
    debug: Color,
    disabled: Color,
    focus: Color,
    canvas: Color,
    warning: Color,
    warning_ink: Color,
}

impl Palette {
    fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self {
                status: [237, 237, 235],
                shadow: [223, 223, 220],
                panel: [238, 238, 236],
                rule: [217, 218, 213],
                ink: [106, 106, 103],
                points_ink: [108, 108, 105],
                title_ink: [94, 94, 91],
                debug_ink: [163, 163, 161],
                muted: [110, 111, 105],
                button: [243, 243, 242],
                hover: [251, 251, 251],
                title: [250, 250, 250],
                title_hover: [254, 254, 254],
                debug: [236, 236, 235],
                disabled: [242, 242, 240],
                focus: [111, 117, 116],
                canvas: [228, 228, 226],
                warning: [247, 235, 213],
                warning_ink: [119, 65, 29],
            },
            Theme::Dark => Self {
                status: [27, 27, 29],
                shadow: [30, 30, 32],
                panel: [37, 37, 38],
                rule: [61, 61, 65],
                ink: [163, 163, 159],
                points_ink: [193, 193, 189],
                title_ink: [212, 212, 207],
                debug_ink: [113, 113, 111],
                muted: [173, 173, 168],
                button: [54, 54, 56],
                hover: [67, 67, 69],
                title: [54, 54, 56],
                title_hover: [73, 73, 73],
                debug: [48, 48, 50],
                disabled: [44, 44, 45],
                focus: [142, 147, 146],
                canvas: [42, 42, 44],
                warning: [58, 44, 31],
                warning_ink: [239, 182, 130],
            },
        }
    }
}

struct Draw<'a, 'b> {
    painter: &'a mut Painter<'b>,
    scale: f64,
    colors: Palette,
    hover: Option<Action>,
    focus: Option<Action>,
}

impl Draw<'_, '_> {
    fn rect(&mut self, r: Rect, color: Color) {
        let s = self.scale;
        self.painter.rect(r.x * s, r.y * s, r.w * s, r.h * s, color);
    }

    fn round(&mut self, r: Rect, radius: f64, color: Color) {
        let s = self.scale;
        self.painter
            .rounded(r.x * s, r.y * s, r.w * s, r.h * s, radius * s, color);
    }

    fn outline(&mut self, r: Rect, radius: f64, color: Color) {
        let s = self.scale;
        self.painter
            .outline(r.x * s, r.y * s, r.w * s, r.h * s, radius * s, s, color);
    }

    fn button(&mut self, rect: Rect, action: Action, enabled: bool, radius: f64) -> Color {
        let hover = self.hover == Some(action);
        let color = if !enabled {
            self.colors.disabled
        } else {
            match (action, hover) {
                (Action::Shop, false) => self.colors.title,
                (Action::Shop, true) => self.colors.title_hover,
                (Action::Debug, false) => self.colors.debug,
                (_, false) => self.colors.button,
                (_, true) => self.colors.hover,
            }
        };
        self.round(rect, radius, color);
        if enabled && self.focus == Some(action) {
            self.outline(
                Rect::new(rect.x - 2.0, rect.y - 2.0, rect.w + 4.0, rect.h + 4.0),
                radius + 2.0,
                self.colors.focus,
            );
        }
        color
    }

    fn label_button(&mut self, rect: Rect, text: &str, action: Action, enabled: bool) {
        let title = matches!(action, Action::Shop | Action::Debug);
        let bg = self.button(rect, action, enabled, if title { 8.0 } else { 7.0 });
        let ink = if !enabled {
            blend(bg, self.colors.ink, 45, 100)
        } else {
            match action {
                Action::Shop => self.colors.title_ink,
                Action::Debug if self.hover != Some(action) => self.colors.debug_ink,
                _ => self.colors.ink,
            }
        };
        let size = 12.0;
        let tracking = if title { 0.48 } else { 0.0 };
        let x = rect.x + ((rect.w - tracked_width(text, size, tracking)) / 2.0).max(2.0);
        self.text_tracked(
            text,
            x,
            rect.y + (rect.h - 14.0) / 2.0 + if title { 1.0 } else { 0.0 },
            size,
            ink,
            bg,
            Some(rect),
            tracking,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        text: &str,
        x: f64,
        y: f64,
        size: f64,
        ink: Color,
        bg: Color,
        clip: Option<Rect>,
    ) {
        self.text_tracked(text, x, y, size, ink, bg, clip, 0.0);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_tracked(
        &mut self,
        text: &str,
        mut x: f64,
        y: f64,
        size: f64,
        ink: Color,
        bg: Color,
        clip: Option<Rect>,
        tracking: f64,
    ) {
        if clip.is_some_and(|r| y + size + 4.0 <= r.y || y >= r.bottom()) {
            return;
        }
        let font = Font::new(size);
        let unit = font.unit;
        let colors = [bg, blend(bg, ink, 1, 3), blend(bg, ink, 2, 3), ink];
        let raster_y = (y * self.scale).round() / self.scale;
        for ch in text.chars() {
            if clip.is_some_and(|r| x >= r.right()) {
                break;
            }
            if let Some(glyph) = font.glyph(ch).or_else(|| font.glyph('?')) {
                let raster_x = (x * self.scale).round() / self.scale;
                let (runs, _) = glyph.runs.as_chunks::<2>();
                for &run in runs {
                    let packed = u16::from_le_bytes(run);
                    let left = f64::from(packed & 15) - 1.0;
                    let top = f64::from((packed >> 4) & 31);
                    let width = f64::from(((packed >> 9) & 15) + 1);
                    let coverage = usize::from((packed >> 13) & 3);
                    let rect = Rect::new(
                        raster_x + left * unit,
                        raster_y + top * unit,
                        width * unit,
                        unit,
                    );
                    if let Some(bounds) = clip {
                        if let Some(rect) = bounds.intersection(rect) {
                            self.rect(rect, colors[coverage]);
                        }
                    } else {
                        self.rect(rect, colors[coverage]);
                    }
                }
                x += glyph.advance * unit + tracking;
            }
        }
    }

    fn icon(&mut self, rect: Rect, action: Action, theme: Theme, sound: bool, bg: Color) {
        let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        let ink = self.colors.ink;
        match action {
            Action::Theme => match theme {
                Theme::Light => {
                    self.round(Rect::new(x - 6.4, y - 6.0, 12.4, 12.4), 6.2, ink);
                    self.round(Rect::new(x - 1.6, y - 8.3, 10.4, 10.4), 5.2, bg);
                }
                Theme::Dark => {
                    self.round(Rect::new(x - 3.3, y - 3.3, 6.6, 6.6), 3.3, ink);
                    for (dx, dy) in [
                        (-6.2, 0.0),
                        (6.2, 0.0),
                        (0.0, -6.2),
                        (0.0, 6.2),
                        (-4.5, -4.5),
                        (4.5, -4.5),
                        (-4.5, 4.5),
                        (4.5, 4.5),
                    ] {
                        self.round(Rect::new(x + dx - 0.9, y + dy - 0.9, 1.8, 1.8), 0.9, ink);
                    }
                }
            },
            Action::ZoomOut | Action::ZoomIn => {
                self.round(Rect::new(x - 5.2, y - 0.75, 10.4, 1.5), 0.75, ink);
                if action == Action::ZoomIn {
                    self.round(Rect::new(x - 0.75, y - 5.2, 1.5, 10.4), 0.75, ink);
                }
            }
            Action::Home => {
                self.outline(Rect::new(x - 5.3, y - 5.3, 10.6, 10.6), 5.3, ink);
                self.round(Rect::new(x - 1.25, y - 1.25, 2.5, 2.5), 1.25, ink);
            }
            Action::Sound => {
                self.round(Rect::new(x - 7.5, y - 2.3, 4.8, 4.6), 0.8, ink);
                for i in 0..5 {
                    let i = f64::from(i);
                    self.rect(
                        Rect::new(x - 4.9 + i, y - 2.0 - i * 0.76, 1.0, 4.0 + i * 1.52),
                        ink,
                    );
                }
                for i in -3i32..=3 {
                    let dy = f64::from(i);
                    if sound {
                        self.rect(
                            Rect::new(x + 5.5 - dy * dy * 0.16, y + dy - 0.5, 1.25, 1.0),
                            ink,
                        );
                        if i.abs() <= 1 {
                            self.rect(
                                Rect::new(x + 2.8 - dy.abs() * 0.5, y + dy - 0.5, 1.0, 1.0),
                                ink,
                            );
                        }
                    } else if i.abs() <= 2 {
                        for dx in [-dy, dy] {
                            self.rect(Rect::new(x + 4.3 + dx, y + dy - 0.55, 1.15, 1.15), ink);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn item(&mut self, rect: Rect) {
        // A tiny pointer with two trailing marks, drawn without a platform emoji font.
        const POINTER: [u16; 13] = [
            0b0000000000001,
            0b0000000000011,
            0b0000000000101,
            0b0000000001001,
            0b0000000010001,
            0b0000000100001,
            0b0000001000001,
            0b0000011110001,
            0b0000100011001,
            0b0001000110111,
            0b0001000100000,
            0b0000111000000,
            0b0000000000000,
        ];
        let (x, y) = (rect.x + 4.0, rect.y + 3.0);
        for (dy, bits) in POINTER.into_iter().enumerate() {
            for dx in 0..10 {
                if bits & (1 << dx) != 0 {
                    self.rect(
                        Rect::new(x + f64::from(dx), y + dy as f64, 1.0, 1.0),
                        self.colors.ink,
                    );
                }
            }
        }
        self.round(
            Rect::new(x + 10.0, y + 10.0, 4.3, 1.1),
            0.55,
            self.colors.muted,
        );
        self.round(
            Rect::new(x + 8.0, y + 13.0, 4.4, 1.1),
            0.55,
            self.colors.muted,
        );
    }
}

fn paint_hint(draw: &mut Draw<'_, '_>, layout: &Layout<'_>) {
    const HINT: &str = "クリックで返す　／　背景をドラッグで移動　／　ホイールで拡大縮小";
    let w = text_width(HINT, 12.0);
    let left_of_controls = layout.controls.first().map_or(layout.w, |(_, r)| r.x);
    let centered = (layout.w - w) / 2.0;
    let colors = draw.colors;
    if layout.w > 640.0 && centered + w + 8.0 <= left_of_controls {
        draw.text(
            HINT,
            centered,
            layout.h - 36.0,
            12.0,
            colors.muted,
            colors.canvas,
            None,
        );
    } else {
        let width = layout.w - 32.0;
        let (mut offset, mut x, mut lines) = (0, 0.0, Vec::new());
        for (at, ch) in HINT.char_indices() {
            let next = text_width(ch.encode_utf8(&mut [0; 4]), 12.0);
            if x > 0.0 && x + next > width {
                lines.push(&HINT[offset..at]);
                offset = at;
                x = 0.0;
            }
            x += next;
        }
        lines.push(&HINT[offset..]);
        let top = layout.h - 56.0 - lines.len() as f64 * 16.0;
        for (i, line) in lines.into_iter().enumerate() {
            draw.text(
                line,
                16.0,
                top + i as f64 * 16.0,
                12.0,
                colors.muted,
                colors.canvas,
                None,
            );
        }
    }
}

fn paint_tooltip(draw: &mut Draw<'_, '_>, layout: &Layout<'_>, action: Action, [x, y]: [f64; 2]) {
    let title = match action {
        Action::Sound => "効果音 オン/オフ",
        Action::Theme => "ダークモード オン/オフ",
        Action::ZoomOut => "縮小",
        Action::ZoomIn => "拡大",
        Action::Home => "開始位置へ戻る",
        Action::Block => "タイルを押したままなぞると、めくれます",
        _ => return,
    };
    let width = (text_width(title, 12.0) + 14.0).min(layout.w - 12.0);
    let height = 23.0;
    let x = (x + 10.0).clamp(6.0, layout.w - width - 6.0);
    let below = y + 18.0;
    let y = if below + height + 6.0 <= layout.h {
        below
    } else {
        (y - height - 8.0).max(6.0)
    };
    let rect = Rect::new(x, y, width, height);
    let colors = draw.colors;
    draw.round(rect, 4.0, colors.title);
    draw.outline(rect, 4.0, colors.rule);
    draw.text(
        title,
        x + 7.0,
        y + 5.0,
        12.0,
        colors.title_ink,
        colors.title,
        Some(rect),
    );
}

fn blend(bg: Color, fg: Color, p: u16, q: u16) -> Color {
    std::array::from_fn(|i| ((u16::from(bg[i]) * (q - p) + u16::from(fg[i]) * p + q / 2) / q) as u8)
}

// Unicode-indexed, antialiased horizontal row runs at three native sizes.
// Both painters use the exact same licensed subset (see assets/hud-glyphs.py).
const GLYPHS: &[u8] = include_bytes!("../../assets/hud-glyphs.bin");

struct Glyph {
    advance: f64,
    runs: &'static [u8],
}

struct Font {
    data: &'static [u8],
    unit: f64,
}

impl Font {
    fn new(size: f64) -> Self {
        let (index, em) = if size <= 12.0 {
            (0, 12.0)
        } else if size <= 13.0 {
            (4, 13.0)
        } else {
            (8, 15.0)
        };
        let offset = u32::from_le_bytes(GLYPHS[index..index + 4].try_into().unwrap()) as usize;
        Self {
            data: &GLYPHS[offset..],
            unit: size / em,
        }
    }

    fn glyph(&self, ch: char) -> Option<Glyph> {
        let word = |offset: usize| u16::from_le_bytes([self.data[offset], self.data[offset + 1]]);
        let count = usize::from(word(0));
        let (mut lo, mut hi) = (0, count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let entry = 2 + mid * 8;
            match u32::from(word(entry)).cmp(&(ch as u32)) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let start = 2 + count * 8 + usize::from(word(entry + 4)) * 2;
                    let end = start + usize::from(word(entry + 6)) * 2;
                    return Some(Glyph {
                        advance: f64::from(word(entry + 2)) / 64.0,
                        runs: &self.data[start..end],
                    });
                }
            }
        }
        None
    }
}

fn text_width(text: &str, size: f64) -> f64 {
    let font = Font::new(size);
    text.chars()
        .filter_map(|ch| font.glyph(ch).or_else(|| font.glyph('?')))
        .map(|glyph| glyph.advance * font.unit)
        .sum()
}

fn tracked_width(text: &str, size: f64, tracking: f64) -> f64 {
    text_width(text, size) + text.chars().count() as f64 * tracking
}

#[cfg(test)]
mod tests {
    use super::*;
    use _8times8::{CellIndex, Target};

    const DESKTOP: PhysicalSize<u32> = PhysicalSize::new(840, 640);

    fn center(rect: Rect, dpi: f64) -> [f64; 2] {
        [(rect.x + rect.w / 2.0) * dpi, (rect.y + rect.h / 2.0) * dpi]
    }

    fn row_button(hud: &Hud, size: PhysicalSize<u32>, action: Action) -> Option<Rect> {
        let view = hud.layout(size, 1.0)?.shop?;
        hud.shop_lines.iter().find_map(|line| match line {
            ShopLine::Row(y, row) if row.action == action => view.button(*y, &row.caption),
            _ => None,
        })
    }

    #[test]
    fn starts_at_zero_and_only_hud_picks_off_board_events() {
        let mut hud = Hud::default();
        hud.update(&Game::new());
        let layout = hud.layout(DESKTOP, 1.0).unwrap();
        assert_eq!(layout.points, [("0A", 24.0, 23.0)]);
        assert!((layout.status.w - 102.4).abs() < 0.01);
        assert_eq!(layout.status.h, 74.0);
        assert_eq!(layout.shop_button.y, 48.0);
        assert!(layout.item.is_none());
        assert_eq!(hud.hit(DESKTOP, 1.0, [24.0, 27.0]), Some(Action::Block));
        assert_eq!(hud.hit(DESKTOP, 1.0, [50.0, 57.0]), Some(Action::Shop));
        assert_eq!(hud.hit(DESKTOP, 1.0, [790.0, 25.0]), Some(Action::Debug));
        assert_eq!(hud.hit(DESKTOP, 1.0, [419.0, 320.0]), None);
        assert!(!hud.scroll(DESKTOP, 1.0, [419.0, 320.0], -40.0));
        assert!(hud.scroll(DESKTOP, 1.0, [50.0, 57.0], -40.0));
        assert_eq!(hud.hit(DESKTOP, 0.0, [50.0, 57.0]), None);
        assert_eq!(hud.hit(PhysicalSize::new(0, 640), 1.0, [50.0, 57.0]), None);
        hud.set_machine_rows(vec![MachineTier {
            tier: 1,
            owned: 0,
            limit: 4,
            legacy: 0,
            body: Some(MachineRow {
                id: 1,
                current: capacity_label(0),
                next: None,
                cost: "80A".into(),
                enabled: false,
            }),
            machines: Vec::new(),
        }]);
        hud.toggle_shop();
        let panel = hud.layout(DESKTOP, 1.0).unwrap().shop.unwrap().rect;
        assert_eq!(
            [panel.x, panel.y, panel.w, panel.h],
            [16.0, 92.0, 340.0, 192.0]
        );
    }

    #[test]
    fn controls_stay_pinned_and_hit_test_at_new_size_and_dpi() {
        let mut hud = Hud::default();
        assert_eq!(hud.hit(DESKTOP, 1.0, [638.0, 606.0]), None);
        hud.set_sound(Some(false));
        for (size, dpi) in [(DESKTOP, 1.0), (PhysicalSize::new(720, 540), 1.5)] {
            let bottom = f64::from(size.height) - 34.0 * dpi;
            for (i, action) in [
                Action::Home,
                Action::ZoomIn,
                Action::ZoomOut,
                Action::Theme,
                Action::Sound,
            ]
            .into_iter()
            .enumerate()
            {
                let right = f64::from(size.width) - (34.0 + i as f64 * 42.0) * dpi;
                assert_eq!(hud.hit(size, dpi, [right, bottom]), Some(action));
                assert_eq!(hud.tooltip_action(size, dpi, [right, bottom]), Some(action));
            }
            let gap = f64::from(size.width) - 55.0 * dpi;
            assert_eq!(hud.hit(size, dpi, [gap, bottom]), Some(Action::Block));
            assert_eq!(hud.tooltip_action(size, dpi, [gap, bottom]), None);
        }
    }

    #[test]
    fn autosave_warning_stays_visible_and_keeps_shop_clear() {
        let mut hud = Hud::default();
        assert!(hud.layout(DESKTOP, 1.0).unwrap().save_warning.is_none());
        hud.set_save_blocked(true);
        hud.update(&Game::new());
        hud.toggle_shop();
        let tight = PhysicalSize::new(480, 360);
        let layout = hud.layout(tight, 1.0).unwrap();
        let badge = layout.save_warning.unwrap();
        let shop = layout.shop.unwrap().rect;
        assert!(layout.status.fits(badge));
        assert!(shop.y > badge.bottom());
        assert_eq!(hud.hit(tight, 1.0, center(badge, 1.0)), Some(Action::Block));
        hud.toggle_debug();
        let debug = hud.layout(tight, 1.0).unwrap().debug.unwrap();
        assert!(debug.view.rect.intersection(badge).is_none());
    }

    #[test]
    fn shared_painter_receives_counter_and_japanese_button_glyphs() {
        let game = Game::new();
        let mut hud = Hud::default();
        hud.update(&game);
        hud.set_hint(false);
        let button = hud.layout(DESKTOP, 1.0).unwrap().shop_button;
        for theme in [Theme::Light, Theme::Dark] {
            let mut instances = Vec::new();
            let mut painter = Painter {
                output: &mut instances,
                size: DESKTOP,
                limit: 16_384,
            };
            hud.paint(&mut painter, &game, theme, 1.0, None);
            let painted_runs = |bounds: Rect| {
                instances
                    .iter()
                    .filter(|instance| {
                        let [x, y, _, _] = instance.rect;
                        let point = [
                            f64::from(x + 1.0) * f64::from(DESKTOP.width) / 2.0,
                            f64::from(1.0 - y) * f64::from(DESKTOP.height) / 2.0,
                        ];
                        instance.shape[1] <= 1.0 && bounds.contains(point)
                    })
                    .count()
            };
            assert!(
                painted_runs(Rect::new(23.0, 18.0, 25.0, 20.0)) > 20,
                "missing 0A counter"
            );
            assert!(painted_runs(button) > 80, "missing Japanese shop action");
        }
    }

    #[test]
    fn higher_balances_and_item_are_visible_even_after_spending_the_bonus() {
        let mut game = Game::new();
        assert!(game.add_debug_bonus(Level::B, &Amount::from(3)));
        game.set_drag_item(true);
        let mut hud = Hud::default();
        hud.update(&game);
        let layout = hud.layout(DESKTOP, 1.0).unwrap();
        assert_eq!(
            layout.points.iter().map(|(s, _, _)| *s).collect::<Vec<_>>(),
            ["3B", "0A"]
        );
        let icon = layout.item.unwrap();
        assert_eq!(
            hud.tooltip_action(DESKTOP, 1.0, center(icon, 1.0)),
            Some(Action::Block)
        );
        assert_eq!(
            hud.hit(DESKTOP, 1.0, center(icon, 1.0)),
            Some(Action::Block)
        );
        assert!(game.try_spend(Level::B, &Amount::from(3)));
        hud.update(&game);
        assert_eq!(hud.layout(DESKTOP, 1.0).unwrap().points[0].0, "0B");
    }

    #[test]
    fn manual_price_and_enabled_state_follow_earned_and_spent_balance() {
        let mut game = Game::new();
        let mut hud = Hud::default();
        hud.toggle_shop();
        hud.update(&game);
        let button = center(row_button(&hud, DESKTOP, Action::Purchase).unwrap(), 1.0);
        assert_eq!(hud.hit(DESKTOP, 1.0, button), Some(Action::Block));
        let manual = hud.manual.as_ref().unwrap();
        assert_eq!(manual.caption, "強化 8A");
        assert_eq!(manual.description, "現在：1枚／回 → 次：2枚／回");
        assert_eq!(hud.tab(DESKTOP, 1.0, false), Some(Action::Shop));
        assert_eq!(hud.tab(DESKTOP, 1.0, false), Some(Action::Debug));

        assert!(game.apply(
            Target::Tile {
                x: 0,
                y: 0,
                cell: CellIndex::new(0).unwrap()
            },
            8 * 64
        ));
        hud.update(&game);
        assert_eq!(hud.layout(DESKTOP, 1.0).unwrap().points[0].0, "8A");
        assert_eq!(hud.hit(DESKTOP, 1.0, button), Some(Action::Purchase));
        assert!(game.try_upgrade());
        hud.update(&game);
        assert_eq!(hud.layout(DESKTOP, 1.0).unwrap().points[0].0, "0A");
        assert_eq!(hud.hit(DESKTOP, 1.0, button), Some(Action::Block));
        assert_eq!(
            hud.manual.as_ref().unwrap().description,
            "現在：2枚／回 → 次：4枚／回"
        );
    }

    #[test]
    fn machine_rows_scroll_and_tab_preserves_tier_and_id() {
        let tiny = PhysicalSize::new(320, 240);
        let machine = |id, enabled| MachineRow {
            id,
            current: capacity_label(0),
            next: Some(capacity_label(1)),
            cost: "20A".into(),
            enabled,
        };
        let mut hud = Hud::default();
        hud.update(&Game::new());
        hud.set_machine_rows(vec![MachineTier {
            tier: 1,
            owned: 4,
            limit: 4,
            legacy: 0,
            body: Some(machine(0, true)),
            machines: vec![
                machine(42, true),
                machine(77, false),
                machine(81, true),
                machine(99, true),
            ],
        }]);
        hud.toggle_shop();
        let first = Action::MachineUpgrade(1, 42);
        let last = Action::MachineUpgrade(1, 99);
        assert!(row_button(&hud, DESKTOP, Action::MachineBody(1)).is_none());
        let disabled = center(
            row_button(&hud, DESKTOP, Action::MachineUpgrade(1, 77)).unwrap(),
            1.0,
        );
        assert_eq!(hud.hit(DESKTOP, 1.0, disabled), Some(Action::Block));
        assert!(row_button(&hud, tiny, first).is_none());
        assert_eq!(hud.tab(tiny, 1.0, false), Some(Action::Shop));
        assert_eq!(hud.tab(tiny, 1.0, false), Some(first));
        let pos = center(row_button(&hud, tiny, first).unwrap(), 1.0);
        assert_eq!(hud.hit(tiny, 1.0, pos), Some(first));
        assert!(hud.scroll(tiny, 1.0, [50.0, 120.0], -1_000.0));
        let pos = center(row_button(&hud, tiny, last).unwrap(), 1.0);
        assert_eq!(hud.hit(tiny, 1.0, pos), Some(last));
        assert!(row_button(&hud, tiny, first).is_none());
        hud.toggle_shop();
        assert_eq!(hud.hit(tiny, 1.0, pos), None);
        assert!(hud.focused_action().is_none());
    }

    #[test]
    fn debug_wheel_and_confirmation_have_separate_targets() {
        let tiny = PhysicalSize::new(320, 240);
        let mut hud = Hud::default();
        hud.update(&Game::new());
        hud.toggle_debug();
        let find = |hud: &Hud, action| {
            let debug = hud.layout(tiny, 1.0).unwrap().debug.unwrap();
            debug.buttons().find_map(|(a, _, rect)| {
                (a == action && debug.view.clip.fits(rect)).then_some(center(rect, 1.0))
            })
        };
        let reset = Action::DebugCommand(DebugAction::Reset);
        assert_eq!(hud.hit(tiny, 1.0, find(&hud, reset).unwrap()), Some(reset));
        let paste = Action::DebugCommand(DebugAction::PasteSave);
        assert!(find(&hud, paste).is_none());
        assert!(hud.scroll(tiny, 1.0, [100.0, 100.0], -1_000.0));
        assert_eq!(hud.hit(tiny, 1.0, find(&hud, paste).unwrap()), Some(paste));
        assert!(find(&hud, reset).is_none());

        hud.set_debug_pending(Some(DebugAction::PasteSave));
        assert_eq!(hud.focused_action(), Some(Action::DebugCancel));
        assert_eq!(hud.hit(tiny, 1.0, [160.0, 200.0]), Some(Action::Block));
        assert_eq!(hud.tab(tiny, 1.0, false), Some(Action::DebugConfirm));
        let confirm = find(&hud, Action::DebugConfirm).unwrap();
        assert_eq!(hud.hit(tiny, 1.0, confirm), Some(Action::DebugConfirm));
        hud.set_debug_pending(None);
        assert_ne!(hud.hit(tiny, 1.0, confirm), Some(Action::DebugConfirm));
        assert_eq!(hud.hit(tiny, 1.0, find(&hud, paste).unwrap()), Some(paste));
    }

    #[test]
    fn atlas_covers_all_japanese_labels_and_number_formatting() {
        assert_eq!(points_label(999_999_999, Level::A), "999,999,999A");
        assert_eq!(points_label(1_021_000_000, Level::B), "10.21億B");
        assert_eq!(
            points_label(100_000_000_000_000_000_000u128, Level::E),
            "1垓E"
        );
        assert_eq!(points_label(u128::MAX, Level::H), "3.40e38H");
        assert_eq!(capacity_label(5), "32枚");
        assert_eq!(capacity_label(6), "1A");
        assert_eq!(capacity_label(11), "32A");
        assert_eq!(capacity_label(24), "1D");
        for size in [12.0, 13.0, 15.0] {
            let font = Font::new(size);
            for ch in include_str!("hud.rs").chars().filter(|ch| !ch.is_ascii()) {
                let glyph = font
                    .glyph(ch)
                    .unwrap_or_else(|| panic!("missing HUD glyph: {ch}"));
                assert!(
                    ch.is_whitespace() || !glyph.runs.is_empty(),
                    "empty HUD glyph: {ch}"
                );
            }
        }
    }
}
