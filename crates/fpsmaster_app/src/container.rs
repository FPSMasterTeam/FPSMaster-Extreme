//! Server-driven container windows — a faithful port of vanilla 1.8.9
//! `Container.slotClick` / `mergeItemStack` (MCP-919). Clicks predict locally
//! exactly like the client's `playerController.windowClick` and emit the
//! matching C0E ClickWindow packets; the server confirms or corrects.
//!
//! The player inventory (window 0) is modelled as `WindowKind::Player`; chests,
//! dispensers, furnaces, etc. are server-opened windows whose lower 36 slots
//! alias the shared player inventory, exactly like vanilla's slot list mixing a
//! container `IInventory` with `InventoryPlayer`.

use fpsmaster_protocol::io::PacketReader;
use fpsmaster_protocol::v1_8_9::packets::{read_slot, ServerboundPacket, SlotItem};

/// One villager trade offer (vanilla `MerchantRecipe`), parsed from the
/// `MC|TrList` plugin message. The interactive slots are still server-driven;
/// these recipes only feed the trade preview row and the `< >` selection.
#[derive(Debug, Clone)]
pub struct MerchantRecipe {
    /// The first (primary) item the trade costs.
    pub buy: SlotItem,
    /// The optional second cost item.
    pub buy_secondary: Option<SlotItem>,
    /// The item the trade yields.
    pub sell: SlotItem,
    /// Whether the trade is locked out (vanilla `toolUses >= maxTradeUses`).
    pub disabled: bool,
}

/// Parse a `MC|TrList` payload (vanilla `MerchantRecipeList.readFromBuf`,
/// preceded by the window id): `int window_id`, `byte count`, then per recipe
/// `buy, sell, bool hasSecond, [buy2], bool disabled, int uses, int maxUses`.
/// Returns the window id and the offers, or `None` if the bytes are malformed.
pub fn parse_trade_list(data: &[u8]) -> Option<(i32, Vec<MerchantRecipe>)> {
    let mut r = PacketReader::new(data);
    let window_id = r.read_i32().ok()?;
    let count = r.read_u8().ok()?;
    let mut trades = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let buy = read_slot(&mut r).ok()??;
        let sell = read_slot(&mut r).ok()??;
        let buy_secondary = if r.read_bool().ok()? {
            read_slot(&mut r).ok()?
        } else {
            None
        };
        let disabled = r.read_bool().ok()?;
        let _uses = r.read_i32().ok()?;
        let _max_uses = r.read_i32().ok()?;
        trades.push(MerchantRecipe { buy, buy_secondary, sell, disabled });
    }
    Some((window_id, trades))
}

/// Which backing inventory a window slot reads/writes: the container's own
/// `IInventory`, or the shared 45-slot player inventory.
#[derive(Debug, Clone, Copy)]
enum SlotBack {
    Container(usize),
    Player(usize),
}

/// The vanilla `Slot` subclass behaviour we model for client prediction.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SlotKind {
    /// Plain storage slot (any item, stacks to 64).
    Normal,
    /// Result slot (craft/smelt/brew output): items can't be placed INTO it.
    Output,
    /// Single-item slot (armor, enchant input, brewing bottle).
    Limit1,
}

/// One window slot: its backing inventory, GUI position (relative to the window
/// origin) and behaviour.
#[derive(Debug)]
pub struct Slot {
    back: SlotBack,
    pub x: i32,
    pub y: i32,
    kind: SlotKind,
}

impl Slot {
    /// Vanilla `Slot.isItemValid`: can this item be PLACED into the slot.
    fn is_item_valid(&self) -> bool {
        self.kind != SlotKind::Output
    }
    /// Vanilla `Slot.canTakeStack` — always true for the slots we model.
    fn can_take(&self) -> bool {
        true
    }
    /// Vanilla `Slot.getSlotStackLimit`.
    fn stack_limit(&self) -> u8 {
        match self.kind {
            SlotKind::Limit1 => 1,
            _ => 64,
        }
    }
    fn is_player(&self) -> bool {
        matches!(self.back, SlotBack::Player(_))
    }
}

/// Container type — drives the GUI layout/texture and the shift-click ranges.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowKind {
    Player,
    Chest(u8),
    Dispenser,
    Hopper,
    Furnace,
    Crafting,
    Brewing,
    Enchant,
    Anvil,
    Beacon,
    Villager,
}

/// An open window: its slots, own inventory, drag state and transaction counter.
#[derive(Debug)]
pub struct Container {
    pub window_id: u8,
    pub kind: WindowKind,
    pub title: String,
    /// The container's own inventory (empty for the player window).
    container_inv: Vec<Option<SlotItem>>,
    slots: Vec<Slot>,
    /// Window properties (furnace cook/burn, brewing time, enchant levels/seed…).
    /// Vanilla `Container.updateProgressBar`; sized for the widest window
    /// (enchanting: 3 levels + seed + 3 enchant hints).
    pub properties: [i16; 8],
    /// GUI window size in vanilla GUI px.
    pub x_size: i32,
    pub y_size: i32,
    // Vanilla drag (`func_94532_c`/`dragMode`/`dragSlots`) state.
    drag_event: i32,
    drag_mode: i32,
    drag_slots: Vec<usize>,
    action: i16,
    /// Villager trade offers (vanilla `MerchantRecipeList`), set from `MC|TrList`;
    /// empty for non-merchant windows.
    trades: Vec<MerchantRecipe>,
    /// The selected trade index (vanilla `GuiMerchant.selectedMerchantRecipe`).
    selected_trade: usize,
}

impl Container {
    /// The player inventory window (window 0): result, craft grid, armor, main,
    /// hotbar — vanilla `ContainerPlayer` slot order/positions.
    pub fn player() -> Self {
        let mut slots = Vec::with_capacity(45);
        slots.push(Slot { back: SlotBack::Player(0), x: 144, y: 36, kind: SlotKind::Output });
        let craft = [(88, 26), (106, 26), (88, 44), (106, 44)];
        for (i, (x, y)) in craft.into_iter().enumerate() {
            slots.push(Slot { back: SlotBack::Player(1 + i), x, y, kind: SlotKind::Normal });
        }
        for i in 0..4 {
            slots.push(Slot { back: SlotBack::Player(5 + i), x: 8, y: 8 + i as i32 * 18, kind: SlotKind::Limit1 });
        }
        add_player_inv(&mut slots, 84, 142);
        Self::with_slots(0, WindowKind::Player, String::new(), Vec::new(), slots, 176, 166)
    }

    /// A server-opened container window. `container_size` (from OpenWindow) is
    /// the container's own slot count, excluding the 36 player-inventory slots.
    pub fn open(window_id: u8, inventory_type: &str, title: String, container_size: usize) -> Self {
        let kind = WindowKind::from_type(inventory_type, container_size);
        let mut slots = Vec::new();
        let (main_y, hot_y, x_size, y_size) = match kind {
            WindowKind::Chest(rows) => {
                let rows = rows as i32;
                for i in 0..(rows * 9) {
                    slots.push(Slot {
                        back: SlotBack::Container(i as usize),
                        x: 8 + (i % 9) * 18,
                        y: 18 + (i / 9) * 18,
                        kind: SlotKind::Normal,
                    });
                }
                (103 + (rows - 4) * 18, 161 + (rows - 4) * 18, 176, 114 + rows * 18)
            }
            WindowKind::Dispenser => {
                for i in 0..9 {
                    slots.push(Slot {
                        back: SlotBack::Container(i),
                        x: 62 + (i % 3) as i32 * 18,
                        y: 17 + (i / 3) as i32 * 18,
                        kind: SlotKind::Normal,
                    });
                }
                (84, 142, 176, 166)
            }
            WindowKind::Hopper => {
                for i in 0..5 {
                    slots.push(Slot { back: SlotBack::Container(i), x: 44 + i as i32 * 18, y: 20, kind: SlotKind::Normal });
                }
                (51, 109, 176, 133)
            }
            WindowKind::Furnace => {
                slots.push(Slot { back: SlotBack::Container(0), x: 56, y: 17, kind: SlotKind::Normal });
                slots.push(Slot { back: SlotBack::Container(1), x: 56, y: 53, kind: SlotKind::Normal });
                slots.push(Slot { back: SlotBack::Container(2), x: 116, y: 35, kind: SlotKind::Output });
                (84, 142, 176, 166)
            }
            WindowKind::Crafting => {
                slots.push(Slot { back: SlotBack::Container(0), x: 124, y: 35, kind: SlotKind::Output });
                for i in 0..9 {
                    slots.push(Slot {
                        back: SlotBack::Container(1 + i),
                        x: 30 + (i % 3) as i32 * 18,
                        y: 17 + (i / 3) as i32 * 18,
                        kind: SlotKind::Normal,
                    });
                }
                (84, 142, 176, 166)
            }
            WindowKind::Brewing => {
                let pos = [(56, 46), (79, 53), (102, 46)];
                for (i, (x, y)) in pos.into_iter().enumerate() {
                    slots.push(Slot { back: SlotBack::Container(i), x, y, kind: SlotKind::Limit1 });
                }
                slots.push(Slot { back: SlotBack::Container(3), x: 79, y: 17, kind: SlotKind::Normal });
                (84, 142, 176, 166)
            }
            WindowKind::Enchant => {
                for i in 0..container_size.max(1) {
                    slots.push(Slot { back: SlotBack::Container(i), x: 25 + i as i32 * 18, y: 47, kind: SlotKind::Limit1 });
                }
                (84, 142, 176, 166)
            }
            WindowKind::Anvil => {
                // Vanilla `ContainerRepair`: two inputs (left/middle), one output.
                slots.push(Slot { back: SlotBack::Container(0), x: 27, y: 47, kind: SlotKind::Normal });
                slots.push(Slot { back: SlotBack::Container(1), x: 76, y: 47, kind: SlotKind::Normal });
                slots.push(Slot { back: SlotBack::Container(2), x: 134, y: 47, kind: SlotKind::Output });
                (84, 142, 176, 166)
            }
            WindowKind::Beacon => {
                // Vanilla `ContainerBeacon`: a single payment slot; the window is
                // larger than the standard 176×166.
                slots.push(Slot { back: SlotBack::Container(0), x: 136, y: 110, kind: SlotKind::Limit1 });
                (132, 190, 230, 219)
            }
            WindowKind::Villager => {
                // Vanilla `ContainerMerchant`: two buy slots + one sell (output).
                slots.push(Slot { back: SlotBack::Container(0), x: 36, y: 52, kind: SlotKind::Normal });
                slots.push(Slot { back: SlotBack::Container(1), x: 62, y: 52, kind: SlotKind::Normal });
                slots.push(Slot { back: SlotBack::Container(2), x: 120, y: 52, kind: SlotKind::Output });
                (84, 142, 176, 166)
            }
            WindowKind::Player => (84, 142, 176, 166),
        };
        add_player_inv(&mut slots, main_y, hot_y);
        let needed = slots
            .iter()
            .filter_map(|s| match s.back {
                SlotBack::Container(i) => Some(i + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let inv = vec![None; container_size.max(needed)];
        Self::with_slots(window_id, kind, title, inv, slots, x_size, y_size)
    }

    fn with_slots(
        window_id: u8,
        kind: WindowKind,
        title: String,
        container_inv: Vec<Option<SlotItem>>,
        slots: Vec<Slot>,
        x_size: i32,
        y_size: i32,
    ) -> Self {
        Self {
            window_id,
            kind,
            title,
            container_inv,
            slots,
            properties: [0; 8],
            x_size,
            y_size,
            drag_event: 0,
            drag_mode: 0,
            drag_slots: Vec::new(),
            action: 0,
            trades: Vec::new(),
            selected_trade: 0,
        }
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// The villager trade offers (empty until `MC|TrList` arrives).
    pub fn trades(&self) -> &[MerchantRecipe] {
        &self.trades
    }

    /// The currently selected trade index, clamped to the offer list.
    pub fn selected_trade(&self) -> usize {
        self.selected_trade
    }

    /// Replace the trade offers (from `MC|TrList`), keeping the selection valid.
    pub fn set_trades(&mut self, trades: Vec<MerchantRecipe>) {
        if self.selected_trade >= trades.len() {
            self.selected_trade = trades.len().saturating_sub(1);
        }
        self.trades = trades;
    }

    /// Select trade `index`, clamped to the available offers.
    pub fn set_selected_trade(&mut self, index: usize) {
        self.selected_trade = index.min(self.trades.len().saturating_sub(1));
    }

    /// The number of slots that belong to the container itself (not the player
    /// inventory) — used to split a WindowItems / index a SetSlot.
    fn container_size(&self) -> usize {
        self.container_inv.len()
    }

    /// Read a window slot for rendering / engine use.
    fn get(&self, slot: usize, player: &[Option<SlotItem>]) -> Option<SlotItem> {
        match self.slots[slot].back {
            SlotBack::Container(i) => self.container_inv.get(i).cloned().flatten(),
            SlotBack::Player(i) => player.get(i).cloned().flatten(),
        }
    }

    pub fn slot_item(&self, slot: usize, player: &[Option<SlotItem>]) -> Option<SlotItem> {
        if slot < self.slots.len() {
            self.get(slot, player)
        } else {
            None
        }
    }

    fn set(&mut self, slot: usize, player: &mut [Option<SlotItem>], item: Option<SlotItem>) {
        match self.slots[slot].back {
            SlotBack::Container(i) => self.container_inv[i] = item,
            SlotBack::Player(i) => player[i] = item,
        }
    }

    /// Apply a SetSlot for THIS window's id (`slot` is a window slot index).
    pub fn apply_set_slot(&mut self, slot: i16, item: Option<SlotItem>, player: &mut [Option<SlotItem>]) {
        if (0..self.slots.len() as i16).contains(&slot) {
            self.set(slot as usize, player, item);
        }
    }

    /// Apply a WindowItems for THIS window: the first `container_size` items are
    /// the container; the rest (27 main + 9 hotbar) alias the player inventory.
    pub fn apply_window_items(&mut self, items: Vec<Option<SlotItem>>, player: &mut [Option<SlotItem>]) {
        for (i, item) in items.into_iter().enumerate() {
            if i < self.slots.len() {
                self.set(i, player, item);
            }
        }
    }

    /// Apply a WindowProperty (vanilla `Container.updateProgressBar`): store the
    /// field for the GUI (furnace flame/arrow, brewing time, enchant levels…).
    pub fn set_property(&mut self, id: i16, value: i16) {
        if let Some(slot) = self.properties.get_mut(id as usize) {
            *slot = value;
        }
    }

    fn next_action(&mut self) -> i16 {
        self.action = self.action.wrapping_add(1);
        self.action
    }

    /// Vanilla `playerController.windowClick`: run `slotClick` locally and return
    /// the C0E ClickWindow packet to send. `slot_id` is a window slot index, or
    /// -999 for outside the window (drop / drag end).
    pub fn window_click(
        &mut self,
        slot_id: i16,
        button: i8,
        mode: i8,
        player: &mut Vec<Option<SlotItem>>,
        cursor: &mut Option<SlotItem>,
        creative: bool,
    ) -> Vec<ServerboundPacket> {
        let action = self.next_action();
        let clicked = self.slot_click(slot_id, button, mode, player, cursor, creative);
        vec![ServerboundPacket::ClickWindow {
            window_id: self.window_id,
            slot: slot_id,
            button,
            action_number: action,
            mode,
            clicked_item: clicked,
        }]
    }

    fn reset_drag(&mut self) {
        self.drag_event = 0;
        self.drag_slots.clear();
    }

    /// Vanilla `Container.slotClick`, 1:1. Returns the item snapshot the C0E
    /// carries (a copy of the clicked slot for normal/shift clicks).
    fn slot_click(
        &mut self,
        slot_id: i16,
        button: i8,
        mode: i8,
        player: &mut Vec<Option<SlotItem>>,
        cursor: &mut Option<SlotItem>,
        creative: bool,
    ) -> Option<SlotItem> {
        let mut ret = None;

        if mode == 5 {
            // ── Drag (paint) ──────────────────────────────────────────────
            let prev = self.drag_event;
            self.drag_event = (button & 3) as i32; // 0 start, 1 add, 2 end
            if (prev != 1 || self.drag_event != 2) && prev != self.drag_event {
                self.reset_drag();
            } else if cursor.is_none() {
                self.reset_drag();
            } else if self.drag_event == 0 {
                self.drag_mode = ((button >> 2) & 3) as i32; // 0 split, 1 one, 2 fill
                if is_valid_drag_mode(self.drag_mode, creative) {
                    self.drag_event = 1;
                    self.drag_slots.clear();
                } else {
                    self.reset_drag();
                }
            } else if self.drag_event == 1 {
                if slot_id >= 0 && (slot_id as usize) < self.slots.len() {
                    let s = slot_id as usize;
                    let c = cursor.clone().expect("cursor present");
                    let existing = self.get(s, player);
                    if can_add_to_slot(&self.slots[s], existing, &c, true)
                        && self.slots[s].is_item_valid()
                        && c.count as usize > self.drag_slots.len()
                        && !self.drag_slots.contains(&s)
                    {
                        self.drag_slots.push(s);
                    }
                }
            } else if self.drag_event == 2 {
                if !self.drag_slots.is_empty() {
                    let c = cursor.clone().expect("cursor present");
                    let mut remaining = c.count as i32;
                    let n = self.drag_slots.len();
                    let slots = self.drag_slots.clone();
                    for s in slots {
                        let existing = self.get(s, player);
                        if !(can_add_to_slot(&self.slots[s], existing.clone(), &c, true)
                            && self.slots[s].is_item_valid()
                            && c.count as usize >= n)
                        {
                            continue;
                        }
                        let have = existing.map_or(0, |e| e.count as i32);
                        let per = compute_drag_amount(self.drag_mode, &c, n);
                        let limit = max_stack(&c).min(self.slots[s].stack_limit()) as i32;
                        let total = (per + have).min(limit);
                        remaining -= total - have;
                        self.set(s, player, Some(SlotItem { count: total as u8, ..c.clone() }));
                    }
                    *cursor = (remaining > 0).then_some(SlotItem {
                        count: remaining as u8,
                        ..c
                    });
                }
                self.reset_drag();
            } else {
                self.reset_drag();
            }
        } else if self.drag_event != 0 {
            self.reset_drag();
        } else if (mode == 0 || mode == 1) && (button == 0 || button == 1) {
            if slot_id == -999 {
                // ── Drop the cursor outside the window ────────────────────
                if let Some(mut c) = cursor.clone() {
                    if button == 0 {
                        *cursor = None;
                    } else {
                        c.count -= 1;
                        *cursor = (c.count > 0).then_some(c);
                    }
                }
            } else if slot_id < 0 || slot_id as usize >= self.slots.len() {
                // ignore
            } else if mode == 1 {
                // ── Shift-click quick-transfer ────────────────────────────
                let s = slot_id as usize;
                if self.slots[s].can_take() {
                    let before = self.get(s, player);
                    let moved = self.transfer_stack_in_slot(s, player);
                    if let Some(m) = moved {
                        ret = Some(m.clone());
                        // Retry while the slot still holds the same item (vanilla
                        // empties a partially-moved stack with repeated transfers).
                        if before.is_some_and(|b| b.id == m.id) {
                            while let Some(now) = self.get(s, player) {
                                if now.id != m.id {
                                    break;
                                }
                                if self.transfer_stack_in_slot(s, player).is_none() {
                                    break;
                                }
                            }
                        }
                    }
                }
            } else {
                // ── Normal pick / place / swap / merge ────────────────────
                let s = slot_id as usize;
                let slot_stack = self.get(s, player);
                if let Some(ss) = slot_stack.clone() {
                    ret = Some(ss);
                }
                match (cursor.clone(), slot_stack) {
                    (Some(cur), None) => {
                        if self.slots[s].is_item_valid() {
                            let mut take = if button == 0 { cur.count } else { 1 };
                            take = take.min(self.slots[s].stack_limit());
                            self.set(s, player, Some(SlotItem { count: take, ..cur.clone() }));
                            let left = cur.count - take;
                            *cursor = (left > 0).then_some(SlotItem { count: left, ..cur });
                        }
                    }
                    (None, Some(ss)) => {
                        if self.slots[s].can_take() {
                            let take = if button == 0 { ss.count } else { ss.count.div_ceil(2) };
                            *cursor = Some(SlotItem { count: take, ..ss.clone() });
                            let left = ss.count - take;
                            self.set(s, player, (left > 0).then_some(SlotItem { count: left, ..ss }));
                        }
                    }
                    (Some(cur), Some(ss)) => {
                        if self.slots[s].is_item_valid() {
                            if stackable(&cur, &ss) {
                                // Merge cursor into the slot.
                                let cap = self.slots[s].stack_limit().min(max_stack(&cur));
                                let space = cap.saturating_sub(ss.count);
                                let put = if button == 0 { cur.count } else { 1 }.min(space);
                                if put > 0 {
                                    self.set(s, player, Some(SlotItem { count: ss.count + put, ..ss }));
                                    let left = cur.count - put;
                                    *cursor = (left > 0).then_some(SlotItem { count: left, ..cur });
                                }
                            } else if cur.count <= self.slots[s].stack_limit() {
                                // Different items: swap cursor and slot.
                                self.set(s, player, Some(cur));
                                *cursor = Some(ss);
                            }
                        } else if stackable(&cur, &ss)
                            && max_stack(&cur) > 1
                            && ss.count as u16 + cur.count as u16 <= max_stack(&cur) as u16
                            && self.slots[s].can_take()
                        {
                            // Slot rejects placement but the cursor can absorb it
                            // (e.g. picking the result up onto a matching cursor).
                            *cursor = Some(SlotItem { count: cur.count + ss.count, ..cur });
                            self.set(s, player, None);
                        }
                    }
                    (None, None) => {}
                }
            }
        } else if mode == 2 && (0..9).contains(&button) {
            // ── Number-key swap with hotbar slot `button` ─────────────────
            if slot_id >= 0 && (slot_id as usize) < self.slots.len() {
                let s = slot_id as usize;
                let hb = 36 + button as usize; // hotbar slot in the player inventory
                if self.slots[s].can_take() {
                    let slot_stack = self.get(s, player);
                    let hot = player.get(hb).cloned().flatten();
                    // Vanilla only allows the swap when the destination can hold
                    // the displaced stack (same inventory / item valid / a spare).
                    let into_slot_ok = hot.is_none()
                        || (self.slots[s].is_player() && self.slots[s].is_item_valid());
                    if let Some(ss) = slot_stack {
                        if into_slot_ok {
                            player[hb] = Some(ss);
                            self.set(s, player, hot);
                        }
                    } else if let Some(h) = hot {
                        if self.slots[s].is_item_valid() {
                            player[hb] = None;
                            self.set(s, player, Some(h));
                        }
                    }
                }
            }
        } else if mode == 3 && creative && cursor.is_none() && slot_id >= 0 {
            // ── Creative middle-click clone ───────────────────────────────
            if (slot_id as usize) < self.slots.len() {
                if let Some(ss) = self.get(slot_id as usize, player) {
                    *cursor = Some(SlotItem { count: max_stack(&ss), ..ss });
                }
            }
        } else if mode == 4 && cursor.is_none() && slot_id >= 0 {
            // ── Drop from a slot (Q / Ctrl-Q) ─────────────────────────────
            if (slot_id as usize) < self.slots.len() {
                let s = slot_id as usize;
                if self.slots[s].can_take() {
                    if let Some(mut ss) = self.get(s, player) {
                        if button == 0 {
                            ss.count -= 1;
                            self.set(s, player, (ss.count > 0).then_some(ss));
                        } else {
                            self.set(s, player, None);
                        }
                    }
                }
            }
        } else if mode == 6 && slot_id >= 0 && (slot_id as usize) < self.slots.len() {
            // ── Double-click: collect matching items onto the cursor ──────
            let s = slot_id as usize;
            let slot_stack = self.get(s, player);
            if let Some(mut cur) = cursor.clone() {
                let blocked = slot_stack.is_none() || !self.slots[s].can_take();
                if blocked {
                    let len = self.slots.len();
                    // Two passes: first partial stacks, then full ones.
                    for pass in 0..2 {
                        let mut i = 0;
                        while i < len && cur.count < max_stack(&cur) {
                            let idx = if button == 0 { i } else { len - 1 - i };
                            if let Some(other) = self.get(idx, player) {
                                if stackable(&other, &cur)
                                    && self.slots[idx].can_take()
                                    && (pass != 0 || other.count != max_stack(&other))
                                {
                                    let take =
                                        (max_stack(&cur) - cur.count).min(other.count);
                                    cur.count += take;
                                    let left = other.count - take;
                                    self.set(
                                        idx,
                                        player,
                                        (left > 0).then_some(SlotItem { count: left, ..other }),
                                    );
                                }
                            }
                            i += 1;
                        }
                    }
                    *cursor = Some(cur);
                }
            }
        }
        ret
    }

    /// Vanilla `transferStackInSlot` (shift-click), per container kind. Moves the
    /// stack in window slot `s` to the opposite region and returns a copy of the
    /// original stack (or None if nothing moved).
    fn transfer_stack_in_slot(&mut self, s: usize, player: &mut [Option<SlotItem>]) -> Option<SlotItem> {
        let original = self.get(s, player)?;
        let mut stack = original.clone();
        let c = self.container_size();
        let total = self.slots.len();
        let moved = match self.kind {
            WindowKind::Player => self.transfer_player(s, &mut stack, player),
            // Generic container: container slots [0,c), player slots [c,total).
            _ => {
                if s < c {
                    // Container → player (reverse fill, like vanilla containers).
                    self.merge(&mut stack, c, total, true, player)
                } else {
                    // Player → container.
                    self.merge(&mut stack, 0, c, false, player)
                }
            }
        };
        if !moved {
            return None;
        }
        // Write back the shrunk source stack.
        let remaining = stack.count;
        self.set(s, player, if remaining > 0 { Some(stack) } else { None });
        if remaining == original.count {
            // Nothing actually moved (full target) — vanilla returns null.
            return None;
        }
        Some(original)
    }

    /// `ContainerPlayer.transferStackInSlot`: armor/craft → main+hotbar,
    /// hotbar ↔ main. (We don't model the craft-result crafting locally.)
    fn transfer_player(&mut self, s: usize, stack: &mut SlotItem, player: &mut [Option<SlotItem>]) -> bool {
        match s {
            0..=4 => self.merge(stack, 9, 45, false, player), // result/craft → inventory
            5..=8 => self.merge(stack, 9, 45, false, player), // armor → inventory
            9..=35 => {
                // Main → hotbar, else armor where it fits (simplified: hotbar).
                self.merge(stack, 36, 45, false, player)
            }
            36..=44 => self.merge(stack, 9, 36, false, player), // hotbar → main
            _ => false,
        }
    }

    /// Vanilla `Container.mergeItemStack` over window slots [start,end).
    fn merge(
        &mut self,
        stack: &mut SlotItem,
        start: usize,
        end: usize,
        reverse: bool,
        player: &mut [Option<SlotItem>],
    ) -> bool {
        let mut flag = false;
        let order: Vec<usize> = if reverse {
            (start..end).rev().collect()
        } else {
            (start..end).collect()
        };
        // Pass 1: merge into existing same-item stacks.
        if max_stack(stack) > 1 {
            for &i in &order {
                if stack.count == 0 {
                    break;
                }
                if let Some(mut have) = self.get(i, player) {
                    if stackable(&have, stack) {
                        let sum = have.count as u16 + stack.count as u16;
                        let cap = max_stack(stack) as u16;
                        if sum <= cap {
                            stack.count = 0;
                            have.count = sum as u8;
                            self.set(i, player, Some(have));
                            flag = true;
                        } else if (have.count as u16) < cap {
                            stack.count -= (cap - have.count as u16) as u8;
                            have.count = cap as u8;
                            self.set(i, player, Some(have));
                            flag = true;
                        }
                    }
                }
            }
        }
        // Pass 2: drop into the first empty valid slot.
        if stack.count > 0 {
            for &i in &order {
                if self.get(i, player).is_none() && self.slots[i].is_item_valid() {
                    let put = stack.count.min(self.slots[i].stack_limit());
                    let mut new_item = stack.clone();
                    new_item.count = put;
                    self.set(i, player, Some(new_item));
                    stack.count -= put;
                    flag = true;
                    break;
                }
            }
        }
        flag
    }
}

impl WindowKind {
    /// Map an OpenWindow `inventory_type` + container slot count to a kind.
    fn from_type(inventory_type: &str, slots: usize) -> WindowKind {
        let t = inventory_type.trim_start_matches("minecraft:");
        match t {
            "chest" | "container" | "EntityHorse" => WindowKind::Chest((slots.max(1) as u8).div_ceil(9)),
            "dispenser" | "dropper" => WindowKind::Dispenser,
            "hopper" => WindowKind::Hopper,
            "furnace" => WindowKind::Furnace,
            "crafting_table" | "workbench" => WindowKind::Crafting,
            "brewing_stand" => WindowKind::Brewing,
            "enchanting_table" => WindowKind::Enchant,
            "anvil" => WindowKind::Anvil,
            "beacon" => WindowKind::Beacon,
            "villager" => WindowKind::Villager,
            // Unknown types fall back to a chest-shaped grid.
            _ => WindowKind::Chest((slots.max(1) as u8).div_ceil(9)),
        }
    }
}

/// Append the 27 main + 9 hotbar player-inventory slots at the given y offsets.
fn add_player_inv(slots: &mut Vec<Slot>, main_y: i32, hot_y: i32) {
    for i in 0..27 {
        slots.push(Slot {
            back: SlotBack::Player(9 + i),
            x: 8 + (i % 9) as i32 * 18,
            y: main_y + (i / 9) as i32 * 18,
            kind: SlotKind::Normal,
        });
    }
    for i in 0..9 {
        slots.push(Slot {
            back: SlotBack::Player(36 + i),
            x: 8 + i as i32 * 18,
            y: hot_y,
            kind: SlotKind::Normal,
        });
    }
}

/// Vanilla `Container.canAddItemToSlot`: the slot is empty, or holds the same
/// item with room (ignoring the incoming size when `size_matters`).
fn can_add_to_slot(_slot: &Slot, existing: Option<SlotItem>, stack: &SlotItem, size_matters: bool) -> bool {
    match existing {
        None => true,
        Some(have) => {
            stackable(&have, stack)
                && have.count as u16 + if size_matters { 0 } else { stack.count as u16 }
                    <= max_stack(stack) as u16
        }
    }
}

/// `computeStackSize`: the per-slot amount a drag deposits (excluding what's
/// already there). Mode 0 even split, 1 one-each, 2 fill (creative).
fn compute_drag_amount(drag_mode: i32, cursor: &SlotItem, num_slots: usize) -> i32 {
    match drag_mode {
        0 => (cursor.count as i32) / (num_slots.max(1) as i32),
        1 => 1,
        _ => max_stack(cursor) as i32,
    }
}

fn is_valid_drag_mode(mode: i32, creative: bool) -> bool {
    mode == 0 || mode == 1 || (mode == 2 && creative)
}

/// Two stacks merge only when id + damage match (NBT is not modelled).
pub fn stackable(a: &SlotItem, b: &SlotItem) -> bool {
    a.id == b.id && a.damage == b.damage
}

/// Vanilla `Item.getItemStackLimit`: equipment caps at 1, everything else 64.
pub fn max_stack(item: &SlotItem) -> u8 {
    if is_non_stackable(item.id) {
        1
    } else {
        64
    }
}

/// 1.8 item ids that stack to 1 (tools, weapons, armor, buckets, …).
fn is_non_stackable(id: i16) -> bool {
    matches!(
        id,
        256 | 257 | 258 | 259
            | 261
            | 267
            | 268..=279
            | 283..=286
            | 290..=294
            | 298..=317
            | 325..=327
            | 329
            | 333
            | 346
            | 359
            | 373
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Append a slot stack in the vanilla wire form (id i16, count u8, damage
    /// i16, empty NBT) — `None` is the `-1` empty marker.
    fn push_slot(buf: &mut Vec<u8>, item: Option<(i16, u8, i16)>) {
        match item {
            None => buf.extend_from_slice(&(-1i16).to_be_bytes()),
            Some((id, count, damage)) => {
                buf.extend_from_slice(&id.to_be_bytes());
                buf.push(count);
                buf.extend_from_slice(&damage.to_be_bytes());
                buf.push(0); // NBT absent
            }
        }
    }

    /// `MC|TrList` parses in the vanilla field order: buy, sell, hasSecond,
    /// [buy2], disabled, uses, maxUses — with the window id and count up front.
    #[test]
    fn parses_a_trade_list_payload() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&7i32.to_be_bytes()); // window id
        buf.push(2); // two offers
        // Offer 0: 1 emerald -> 1 diamond, with a second cost, enabled.
        push_slot(&mut buf, Some((388, 1, 0)));
        push_slot(&mut buf, Some((264, 1, 0)));
        buf.push(1); // has second item
        push_slot(&mut buf, Some((1, 16, 0)));
        buf.push(0); // not disabled
        buf.extend_from_slice(&3i32.to_be_bytes()); // uses
        buf.extend_from_slice(&7i32.to_be_bytes()); // max uses
        // Offer 1: 2 wheat -> 1 emerald, no second cost, disabled.
        push_slot(&mut buf, Some((296, 2, 0)));
        push_slot(&mut buf, Some((388, 1, 0)));
        buf.push(0); // no second item
        buf.push(1); // disabled
        buf.extend_from_slice(&7i32.to_be_bytes());
        buf.extend_from_slice(&7i32.to_be_bytes());

        let (window_id, trades) = parse_trade_list(&buf).expect("parse");
        assert_eq!(window_id, 7);
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].buy.id, 388);
        assert_eq!(trades[0].sell.id, 264);
        assert_eq!(trades[0].buy_secondary.as_ref().unwrap().id, 1);
        assert!(!trades[0].disabled);
        assert_eq!(trades[1].buy.id, 296);
        assert_eq!(trades[1].buy.count, 2);
        assert!(trades[1].buy_secondary.is_none());
        assert!(trades[1].disabled);
    }

    /// The OpenWindow `inventory_type` string maps to the right kind, and chest
    /// rows derive from the container slot count.
    #[test]
    fn window_type_strings_map_to_kinds() {
        let cases: &[(&str, usize, WindowKind)] = &[
            ("minecraft:crafting_table", 10, WindowKind::Crafting),
            ("minecraft:furnace", 3, WindowKind::Furnace),
            ("minecraft:chest", 27, WindowKind::Chest(3)),
            ("minecraft:chest", 54, WindowKind::Chest(6)),
            ("minecraft:dispenser", 9, WindowKind::Dispenser),
            ("minecraft:dropper", 9, WindowKind::Dispenser),
            ("minecraft:hopper", 5, WindowKind::Hopper),
            ("minecraft:brewing_stand", 4, WindowKind::Brewing),
            ("minecraft:anvil", 3, WindowKind::Anvil),
            ("minecraft:enchanting_table", 2, WindowKind::Enchant),
            ("minecraft:beacon", 1, WindowKind::Beacon),
            ("minecraft:villager", 3, WindowKind::Villager),
            // Unknown types fall back to a chest grid sized from the slot count.
            ("minecraft:unknown_thing", 9, WindowKind::Chest(1)),
        ];
        for &(ty, slots, expected) in cases {
            assert_eq!(WindowKind::from_type(ty, slots), expected, "type {ty}");
        }
    }

    /// Each window lays out its own container slots plus the shared 36
    /// player-inventory slots, with the vanilla container slot count.
    #[test]
    fn window_layouts_have_container_plus_player_slots() {
        // (type, container_size, expected container slot count, x_size, y_size).
        let cases: &[(&str, usize, usize, i32, i32)] = &[
            ("minecraft:crafting_table", 10, 10, 176, 166), // 3×3 grid + result
            ("minecraft:furnace", 3, 3, 176, 166),          // input/fuel/result
            ("minecraft:chest", 27, 27, 176, 168),          // 3 rows
            ("minecraft:chest", 54, 54, 176, 222),          // 6 rows
            ("minecraft:dispenser", 9, 9, 176, 166),        // 3×3
            ("minecraft:hopper", 5, 5, 176, 133),           // 1×5
            ("minecraft:brewing_stand", 4, 4, 176, 166),    // 3 bottles + ingredient
            ("minecraft:anvil", 3, 3, 176, 166),            // 2 inputs + output
            ("minecraft:enchanting_table", 2, 2, 176, 166), // item + lapis
            ("minecraft:beacon", 1, 1, 230, 219),           // payment slot
            ("minecraft:villager", 3, 3, 176, 166),         // buy/buy/sell
        ];
        for &(ty, size, want_container, want_x, want_y) in cases {
            let c = Container::open(1, ty, String::new(), size);
            let container_slots = c.slots().len() - 36; // 36 = main 27 + hotbar 9
            assert_eq!(container_slots, want_container, "container slots for {ty}");
            assert_eq!(c.x_size, want_x, "x_size for {ty}");
            assert_eq!(c.y_size, want_y, "y_size for {ty}");
            // The last 9 slots are the hotbar, the 27 before them the main inv.
            assert_eq!(c.slots().len(), want_container + 36, "total slots for {ty}");
        }
    }
}
